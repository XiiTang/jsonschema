//! A reference chain too long for the stack is refused; it does not abort the process.
// Spawning a thread with a set stack size is unsupported on wasm targets.
#![cfg(not(target_arch = "wasm32"))]
use jsonschema::{error::ValidationErrorKind, Validator};
use serde_json::{json, Map, Value};

/// The stack of each thread that builds or validates here.
const STACK_SIZE: usize = 2 * 1024 * 1024;

/// Definitions `C0` to `C{hops - 1}`: `link` builds each from the reference to the next, and the
/// last is a string.
fn definitions(hops: usize, link: impl Fn(usize, Value) -> Value) -> Map<String, Value> {
    let mut definitions = Map::new();
    for idx in 0..hops {
        let definition = if idx + 1 < hops {
            link(idx, json!({"$ref": format!("#/$defs/C{}", idx + 1)}))
        } else {
            json!({"type": "string"})
        };
        definitions.insert(format!("C{idx}"), definition);
    }
    definitions
}

fn all_of(hops: usize) -> Value {
    let definitions = definitions(hops, |_, next| json!({"allOf": [next]}));
    json!({"$defs": definitions, "$ref": "#/$defs/C0"})
}

fn any_of(hops: usize) -> Value {
    let definitions = definitions(hops, |_, next| json!({"anyOf": [next, {"type": "null"}]}));
    json!({"$defs": definitions, "$ref": "#/$defs/C0"})
}

/// `unevaluatedProperties` at the root, over a chain whose every hop declares a property.
fn unevaluated_at_root(hops: usize) -> Value {
    let mut definitions = Map::new();
    for idx in 0..hops {
        let mut definition = Map::new();
        if idx + 1 < hops {
            definition.insert("$ref".into(), json!(format!("#/$defs/C{}", idx + 1)));
        }
        definition.insert("properties".into(), json!({format!("p{idx}"): true}));
        definitions.insert(format!("C{idx}"), Value::Object(definition));
    }
    json!({"$defs": definitions, "$ref": "#/$defs/C0", "unevaluatedProperties": false})
}

/// `unevaluatedProperties` at every hop.
fn unevaluated_at_every_hop(hops: usize) -> Value {
    let definitions = definitions(
        hops,
        |_, next| json!({"allOf": [next], "unevaluatedProperties": false}),
    );
    json!({"$defs": definitions, "$ref": "#/$defs/C0"})
}

/// Definitions `C0` to `C{hops - 1}`, each a reference to the one before it. The root's first
/// branch lists them in that order under a property, then `last`; its second refers to the last
/// definition. Every target is built before the definition that refers to it, so each definition
/// owns its target and the last one owns the whole chain, while the document and the build stay
/// shallow. A value that is not an object walks the chain once.
fn targets_first(hops: usize, last: Option<Value>) -> Value {
    let mut definitions = Map::new();
    let mut entries = Vec::new();
    for idx in 0..hops {
        let definition = if idx == 0 {
            json!({"type": "string"})
        } else {
            json!({"$ref": format!("#/$defs/C{}", idx - 1)})
        };
        definitions.insert(format!("C{idx}"), definition);
        entries.push(json!({"$ref": format!("#/$defs/C{idx}")}));
    }
    entries.extend(last);
    json!({
        "$defs": definitions,
        "allOf": [
            {"properties": {"each": {"allOf": entries}}},
            {"$ref": format!("#/$defs/C{}", hops - 1)},
        ],
    })
}

fn on_stack<T: Send + 'static>(task: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(STACK_SIZE)
        .spawn(task)
        .expect("the thread starts")
        .join()
        .expect("the thread finishes")
}

fn exhausted(kind: &ValidationErrorKind) -> bool {
    matches!(kind, ValidationErrorKind::StackExhausted)
}

#[test]
fn build_refuses_an_unevaluated_chain_too_long_for_the_stack() {
    for schema in [unevaluated_at_root(5_000), unevaluated_at_every_hop(3_000)] {
        let refused = on_stack(move || match jsonschema::validator_for(&schema) {
            Ok(_) => false,
            Err(error) => exhausted(error.kind()),
        });
        assert!(refused);
    }
}

/// What `validate`, `iter_errors` and `evaluate` answer for `instance`: whether each reports
/// exactly the stack refusal.
fn refuses(validator: &Validator, instance: &Value) {
    let kinds: Vec<bool> = validator
        .iter_errors(instance)
        .map(|error| exhausted(error.kind()))
        .collect();
    assert_eq!(kinds, [true]);
    let first = validator.validate(instance).expect_err("refused");
    assert!(exhausted(first.kind()));
    assert!(!validator.evaluate(instance).flag().valid);
}

#[test]
fn validation_refuses_a_chain_too_long_for_the_stack() {
    let schema = all_of(50_000);
    on_stack(move || {
        let validator = jsonschema::validator_for(&schema).expect("the chain builds");
        refuses(&validator, &json!("chain"));
        refuses(&validator, &json!(1));
        assert!(!validator.is_valid(&json!(1)));
    });
}

#[test]
fn validation_refuses_a_chain_of_targets_built_first() {
    let schema = targets_first(20_000, None);
    on_stack(move || {
        let validator = jsonschema::validator_for(&schema).expect("the chain builds");
        refuses(&validator, &json!("chain"));
        refuses(&validator, &json!(1));
        assert!(!validator.is_valid(&json!(1)));
    });
}

#[test]
fn releasing_a_chain_of_targets_built_first_keeps_the_stack() {
    let schema = targets_first(20_000, None);
    on_stack(move || drop(jsonschema::validator_for(&schema).expect("the chain builds")));
}

#[test]
fn a_failed_build_releases_a_chain_of_targets_built_first() {
    let schema = targets_first(20_000, Some(json!({"$ref": "#/$defs/missing"})));
    assert!(on_stack(move || jsonschema::validator_for(&schema).is_err()));
}

#[test]
fn is_valid_refuses_a_chain_too_long_for_the_stack() {
    let schema = any_of(50_000);
    on_stack(move || {
        let validator = jsonschema::validator_for(&schema).expect("the chain builds");
        assert!(!validator.is_valid(&json!(1)));
    });
}

#[test]
fn a_short_chain_answers_as_before() {
    fn check(validator: &Validator, valid: &Value, invalid: &Value, expected: &str) {
        assert!(validator.is_valid(valid));
        assert!(validator.validate(valid).is_ok());
        assert_eq!(validator.iter_errors(valid).count(), 0);
        assert!(validator.evaluate(valid).flag().valid);
        assert!(!validator.is_valid(invalid));
        let errors: Vec<String> = validator
            .iter_errors(invalid)
            .map(|error| error.to_string())
            .collect();
        assert_eq!(errors, [expected]);
        let first = validator.validate(invalid).expect_err("invalid");
        assert_eq!(first.to_string(), expected);
        assert!(!validator.evaluate(invalid).flag().valid);
    }
    on_stack(|| {
        let string = json!("chain");
        let number = json!(1);
        let validator = jsonschema::validator_for(&all_of(20)).expect("builds");
        check(&validator, &string, &number, r#"1 is not of type "string""#);
        let validator = jsonschema::validator_for(&any_of(20)).expect("builds");
        assert!(validator.is_valid(&string));
        assert!(validator.is_valid(&Value::Null));
        assert!(!validator.is_valid(&number));
        let validator = jsonschema::validator_for(&unevaluated_at_root(20)).expect("builds");
        check(
            &validator,
            &json!({"p0": 1, "p19": 1}),
            &json!({"other": 1}),
            "Unevaluated properties are not allowed ('other' was unexpected)",
        );
        let validator = jsonschema::validator_for(&unevaluated_at_every_hop(20)).expect("builds");
        assert!(validator.is_valid(&string));
        assert!(!validator.is_valid(&number));
    });
}
