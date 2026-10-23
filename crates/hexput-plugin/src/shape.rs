//! Holding a handler's result to its Event's declared return shape (Story 6.6, FR-19).
//!
//! The check runs on the result's wire value — after conversion and the depth check, so it walks
//! bytes the frame already bounds and never a shared or cyclic interpreter value — and reports
//! only the first failure, found in pre-order: at a node its own failure first (its kind; for an
//! object the first shape key the value lacks, in shape order, then the first value key the shape
//! lacks, in value order), then its children in value order, depth first. The walk keeps an
//! explicit stack and never recurses.
//!
//! A Value Secret holder (a map with the key `__secret`) is seen through: its `value` is checked
//! in its place and adds no path segment.

use std::collections::{HashMap, HashSet};

use hexput_exec::wire::{HOLDER_VALUE_KEY, Path, SECRET_KEY};
use hexput_parser::{Shape, ShapeId, ShapeNode};
use hexput_port::Value;

/// One step of the walk: check a value against a shape node (stepping into `segment` first), or
/// step back out of a segment once its subtree is done.
enum Step<'a> {
    Check {
        segment: Option<Segment<'a>>,
        value: &'a Value,
        shape: ShapeId,
    },
    Leave,
}

/// Where a child sits under its parent.
#[derive(Clone, Copy)]
enum Segment<'a> {
    Key(&'a str),
    Index(usize),
}

/// Whether `value` — a handler's result as its fire reply entry would carry it — conforms to
/// `shape` (LANGUAGE-REFERENCE §9): a string, number or boolean node accepts any value of that
/// kind (a MessagePack integer or float is a number); `null` accepts every value and is not
/// descended into; `[]` accepts any array, not descended into; `[t]` an array whose every element
/// conforms to `t`; an object node an object with exactly its keys, each conforming.
///
/// # Errors
///
/// The first failure in pre-order, as `<path> <problem>`: the path rooted at `value` and rendered
/// as every payload path is (`` `value.tags[1]` ``, `` `value["b-c"]` ``), then exactly one of
/// `must be a string|a number|a boolean|an array|an object; found <kind>`, `is missing` or
/// `is not a key of the shape`. `<kind>` is `a string`, `a number`, `a boolean`, `an array`,
/// `an object` or `null` for every value a result converts to; a wire value no conversion
/// produces reads `binary data` or `an extension value`, and a map with a non-string key is
/// `must be an object; found a map with a key that is not a string` at the map's path.
pub fn conform<'a>(value: &'a Value, shape: &'a Shape) -> Result<(), String> {
    let mut path = Path::result();
    let mut stack = vec![Step::Check {
        segment: None,
        value,
        shape: shape.root,
    }];
    while let Some(step) = stack.pop() {
        let (segment, value, id) = match step {
            Step::Leave => {
                path.pop();
                continue;
            }
            Step::Check {
                segment,
                value,
                shape,
            } => (segment, value, shape),
        };
        if let Some(segment) = segment {
            match segment {
                Segment::Key(key) => path.push_key(key),
                Segment::Index(index) => path.push_index(index),
            }
            stack.push(Step::Leave);
        }
        let value = seen_through(value);
        let wrong = |path: &Path<'_>, expected: &str| {
            Err(format!(
                "{} must be {expected}; found {}",
                path.rendered(),
                kind(value)
            ))
        };
        match shape.node(id) {
            ShapeNode::Any => {}
            ShapeNode::String => {
                if !value.is_str() {
                    return wrong(&path, "a string");
                }
            }
            ShapeNode::Number => {
                if !matches!(value, Value::Integer(_) | Value::F32(_) | Value::F64(_)) {
                    return wrong(&path, "a number");
                }
            }
            ShapeNode::Boolean => {
                if !value.is_bool() {
                    return wrong(&path, "a boolean");
                }
            }
            ShapeNode::Array(element) => {
                let Value::Array(items) = value else {
                    return wrong(&path, "an array");
                };
                if let Some(element) = *element {
                    stack.extend(
                        items
                            .iter()
                            .enumerate()
                            .rev()
                            .map(|(index, item)| Step::Check {
                                segment: Some(Segment::Index(index)),
                                value: item,
                                shape: element,
                            }),
                    );
                }
            }
            ShapeNode::Object(fields) => {
                let Value::Map(entries) = value else {
                    return wrong(&path, "an object");
                };
                // One pass over each side, so a wide object costs linear time, never keys × keys.
                let mut names = HashSet::with_capacity(entries.len());
                for (key, _) in entries {
                    let Some(name) = key.as_str() else {
                        // A converted result's object keys are always strings; a map with any
                        // other key is no Hexput object.
                        return Err(format!(
                            "{} must be an object; found a map with a key that is not a string",
                            path.rendered()
                        ));
                    };
                    names.insert(name);
                }
                if let Some((missing, _)) = fields
                    .iter()
                    .find(|(name, _)| !names.contains(name.as_str()))
                {
                    path.push_key(missing);
                    return Err(format!("{} is missing", path.rendered()));
                }
                let declared: HashMap<&str, ShapeId> = fields
                    .iter()
                    .map(|(name, child)| (name.as_str(), *child))
                    .collect();
                let mut children = Vec::with_capacity(entries.len());
                for (key, item) in entries {
                    // Every key is a string: checked above.
                    let name = key.as_str().unwrap_or_default();
                    let Some(&child) = declared.get(name) else {
                        path.push_key(name);
                        return Err(format!("{} is not a key of the shape", path.rendered()));
                    };
                    children.push(Step::Check {
                        segment: Some(Segment::Key(name)),
                        value: item,
                        shape: child,
                    });
                }
                stack.extend(children.into_iter().rev());
            }
        }
    }
    Ok(())
}

/// The value a Value Secret holder carries, in its place; any other value as it is.
fn seen_through(mut value: &Value) -> &Value {
    while let Value::Map(entries) = value {
        if !entries
            .iter()
            .any(|(key, _)| key.as_str() == Some(SECRET_KEY))
        {
            break;
        }
        value = entries
            .iter()
            .find(|(key, _)| key.as_str() == Some(HOLDER_VALUE_KEY))
            .map_or(&Value::Nil, |(_, held)| held);
    }
    value
}

/// How a value's kind reads in a mismatch: `a string`, `a number`, `a boolean`, `an array`,
/// `an object` or `null`.
const fn kind(value: &Value) -> &'static str {
    match value {
        Value::Nil => "null",
        Value::Boolean(_) => "a boolean",
        Value::Integer(_) | Value::F32(_) | Value::F64(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Map(_) => "an object",
        // No result converts to either; named for completeness.
        Value::Binary(_) => "binary data",
        Value::Ext(..) => "an extension value",
    }
}
