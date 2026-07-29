use crate::protocol::v2::ThreadItem;
use codex_protocol::items::CommandExecutionItem;
use codex_protocol::items::TurnItem;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

/// Persisted command item as written before `aggregated_output` existed.
fn persisted_command_item(output_fields: Value) -> TurnItem {
    let mut value = json!({
        "id": "exec-1",
        "command": ["echo", "done"],
        "cwd": "file:///tmp",
        "parsed_cmd": [],
        "source": "agent",
        "status": "completed",
        "exit_code": 0,
    });
    let Value::Object(fields) = output_fields else {
        panic!("output fields must be an object");
    };
    let Value::Object(target) = &mut value else {
        unreachable!("fixture is an object");
    };
    target.extend(fields);

    TurnItem::CommandExecution(
        serde_json::from_value::<CommandExecutionItem>(value).expect("persisted command item"),
    )
}

fn aggregated_output_of(item: TurnItem) -> Option<String> {
    match ThreadItem::from(item) {
        ThreadItem::CommandExecution {
            aggregated_output, ..
        } => aggregated_output,
        other => panic!("expected a command execution item, got {other:?}"),
    }
}

#[test]
fn current_items_use_the_aggregated_output_field() {
    let item = persisted_command_item(json!({
        "stdout": "out\n",
        "stderr": "err\n",
        "aggregated_output": "out\nerr\n",
        "formatted_output": "formatted",
    }));

    assert_eq!(aggregated_output_of(item), Some("out\nerr\n".to_string()));
}

#[test]
fn historical_items_fall_back_to_stdout_and_stderr() {
    let item = persisted_command_item(json!({"stdout": "out\n", "stderr": "err\n"}));

    assert_eq!(aggregated_output_of(item), Some("out\nerr\n".to_string()));
}

#[test]
fn historical_items_fall_back_to_a_single_stream() {
    let stdout_only = persisted_command_item(json!({"stdout": "out\n", "stderr": ""}));
    let stderr_only = persisted_command_item(json!({"stdout": "", "stderr": "err\n"}));

    assert_eq!(
        [
            aggregated_output_of(stdout_only),
            aggregated_output_of(stderr_only)
        ],
        [Some("out\n".to_string()), Some("err\n".to_string())]
    );
}

#[test]
fn historical_items_fall_back_to_formatted_output() {
    let item = persisted_command_item(json!({
        "aggregated_output": "",
        "stdout": "",
        "stderr": "",
        "formatted_output": "formatted\n",
    }));

    assert_eq!(aggregated_output_of(item), Some("formatted\n".to_string()));
}

#[test]
fn items_without_any_output_stay_empty() {
    let item = persisted_command_item(json!({}));

    assert_eq!(aggregated_output_of(item), None);
}
