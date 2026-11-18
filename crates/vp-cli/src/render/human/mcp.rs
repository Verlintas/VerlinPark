//! Human renderer chunk: mcp.

use serde_json::Value;

pub(super) fn print(value: &Value) -> bool {
    if let Some(server_name) = value.get("server_name").and_then(Value::as_str)
        && value.get("tools").is_some()
    {
        let name = value["name"].as_str().unwrap_or("?");
        let transport = value["transport"].as_str().unwrap_or("?");
        let version = value["server_version"].as_str().unwrap_or("");
        let protocol = value["protocol_version"].as_str().unwrap_or("?");
        let resources = value["resources"].as_u64().unwrap_or(0);
        let prompts = value["prompts"].as_u64().unwrap_or(0);
        let resource_note = if value["resources_supported"].as_bool().unwrap_or(false) {
            format!("{resources} resources")
        } else {
            "resources unsupported".to_string()
        };
        let prompt_note = if value["prompts_supported"].as_bool().unwrap_or(false) {
            format!("{prompts} prompts")
        } else {
            "prompts unsupported".to_string()
        };
        println!(
            "{name}  {transport}  {server_name} {version}  protocol={protocol}  {} tools  {resource_note}  {prompt_note}",
            value["tools"].as_u64().unwrap_or(0)
        );
        return true;
    }
    if let Some(server) = value.get("server") {
        println!(
            "{}  {}  {}  {}",
            server["id"].as_str().unwrap_or("?"),
            server["name"].as_str().unwrap_or("?"),
            server["transport"].as_str().unwrap_or("stdio"),
            server["url"]
                .as_str()
                .or_else(|| server["command"].as_str())
                .unwrap_or("?")
        );
        return true;
    }
    if let Some(resources) = value.get("resources").and_then(Value::as_array) {
        for resource in resources {
            println!(
                "{}  <{}>  {}",
                resource["name"].as_str().unwrap_or("?"),
                resource["uri"].as_str().unwrap_or("?"),
                resource["mime_type"].as_str().unwrap_or("")
            );
            if let Some(description) = resource["description"].as_str()
                && !description.is_empty()
            {
                println!("  {description}");
            }
        }
        if let Some(templates) = value.get("templates").and_then(Value::as_array) {
            for template in templates {
                println!(
                    "template {}  <{}>",
                    template["name"].as_str().unwrap_or("?"),
                    template["uri_template"].as_str().unwrap_or("?")
                );
            }
        }
        return true;
    }
    if let Some(contents) = value.get("contents").and_then(Value::as_array) {
        for content in contents {
            if let Some(text) = content["text"].as_str() {
                println!("{}", text);
            } else if let Some(blob) = content["blob"].as_str() {
                println!(
                    "[binary {}: {} base64 chars]",
                    content["uri"].as_str().unwrap_or("?"),
                    blob.len()
                );
            }
        }
        return true;
    }
    if let Some(prompts) = value.get("prompts").and_then(Value::as_array) {
        for prompt in prompts {
            println!(
                "{}  {}",
                prompt["name"].as_str().unwrap_or("?"),
                prompt["description"].as_str().unwrap_or("")
            );
            if let Some(arguments) = prompt["arguments"].as_array() {
                for argument in arguments {
                    println!(
                        "  {}{}",
                        argument["name"].as_str().unwrap_or("?"),
                        if argument["required"].as_bool().unwrap_or(false) {
                            " (required)"
                        } else {
                            ""
                        }
                    );
                }
            }
        }
        return true;
    }
    if let Some(messages) = value.get("messages").and_then(Value::as_array)
        && value.get("server_id").is_some()
    {
        if let Some(description) = value["description"].as_str()
            && !description.is_empty()
        {
            println!("{description}\n");
        }
        for message in messages {
            println!(
                "[{}] {}",
                message["role"].as_str().unwrap_or("user"),
                serde_json::to_string(&message["content"])
                    .unwrap_or_else(|_| message["content"].to_string())
            );
        }
        return true;
    }
    if let Some(tools) = value.get("tools").and_then(Value::as_array) {
        for tool in tools {
            println!(
                "{:<30} {}",
                tool["name"].as_str().unwrap_or("?"),
                tool["description"].as_str().unwrap_or("")
            );
        }
        return true;
    }
    if let Some(result) = value.get("result")
        && value.get("tool").is_some()
    {
        println!(
            "{}",
            serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string())
        );
        return true;
    }
    if let Some(memory_id) = value.get("memory_id").and_then(Value::as_str) {
        println!(
            "memory {} removed={}",
            memory_id,
            value["removed"].as_bool().unwrap_or(false)
        );
        return true;
    }
    if let Some(questions) = value.get("questions").and_then(Value::as_array) {
        for question in questions {
            println!(
                "[{}] {} · {} · {}",
                question["status"].as_str().unwrap_or("?"),
                question["question"].as_str().unwrap_or("?"),
                question["agent_name"].as_str().unwrap_or("?"),
                question["id"].as_str().unwrap_or("?")
            );
            if let Some(options) = question["options"].as_array()
                && !options.is_empty()
            {
                println!(
                    "  options: {}",
                    options
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" | ")
                );
            }
            if let Some(answer) = question["answer"].as_str() {
                println!("  answer: {answer}");
            }
        }
        return true;
    }
    false
}
