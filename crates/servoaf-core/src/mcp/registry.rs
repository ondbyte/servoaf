// Copyright 2026 The servoaf Authors
// SPDX-License-Identifier: MPL-2.0

//! Tool definitions, input schemas, results and argument helpers.

use std::rc::Rc;

use serde_json::{Map, Value, json};

use crate::runtime::LocalFuture;

pub type Args = Map<String, Value>;

/// The result of a `tools/call`, serialized as MCP `CallToolResult`.
#[derive(Debug, Default)]
pub struct ToolResult {
    content: Vec<Value>,
    structured_content: Option<Value>,
    is_error: bool,
}

impl ToolResult {
    pub fn text(text: impl Into<String>) -> Self {
        let mut result = Self::default();
        result.add_text(text);
        result
    }

    /// Pretty JSON as text, plus `structuredContent` when `value` is an object.
    pub fn json(value: Value) -> Self {
        let mut result = Self::text(serde_json::to_string_pretty(&value).unwrap_or_default());
        if value.is_object() {
            result.structured_content = Some(value);
        }
        result
    }

    pub fn image(base64_data: String, mime_type: &str) -> Self {
        let mut result = Self::default();
        result.content.push(json!({"type": "image", "data": base64_data, "mimeType": mime_type}));
        result
    }

    pub fn error(message: impl Into<String>) -> Self {
        let mut result = Self::text(message);
        result.is_error = true;
        result
    }

    pub fn add_text(&mut self, text: impl Into<String>) -> &mut Self {
        self.content.push(json!({"type": "text", "text": text.into()}));
        self
    }

    pub fn is_error(&self) -> bool {
        self.is_error
    }

    pub fn content(&self) -> &[Value] {
        &self.content
    }

    pub fn into_value(self) -> Value {
        let mut result = json!({"content": self.content, "isError": self.is_error});
        if let Some(structured) = self.structured_content {
            result["structuredContent"] = structured;
        }
        result
    }
}

impl<E: Into<String>> From<Result<ToolResult, E>> for ToolResult {
    fn from(result: Result<ToolResult, E>) -> Self {
        result.unwrap_or_else(|error| ToolResult::error(error))
    }
}

pub type ToolHandler<C> = Rc<dyn Fn(Rc<C>, Args) -> LocalFuture<ToolResult>>;

pub struct ToolDefinition<C> {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub handler: ToolHandler<C>,
}

/// Builds a JSON Schema object for tool arguments.
#[derive(Default)]
pub struct SchemaBuilder {
    properties: Map<String, Value>,
    required: Vec<Value>,
}

impl SchemaBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    fn add(mut self, name: &str, property: Value, required: bool) -> Self {
        self.properties.insert(name.to_owned(), property);
        if required {
            self.required.push(Value::from(name));
        }
        self
    }

    fn typed(self, name: &str, kind: &str, description: &str, required: bool) -> Self {
        self.add(name, json!({"type": kind, "description": description}), required)
    }

    pub fn integer(self, name: &str, description: &str) -> Self {
        self.typed(name, "integer", description, false)
    }
    pub fn number(self, name: &str, description: &str) -> Self {
        self.typed(name, "number", description, false)
    }
    pub fn string(self, name: &str, description: &str) -> Self {
        self.typed(name, "string", description, false)
    }
    pub fn boolean(self, name: &str, description: &str) -> Self {
        self.typed(name, "boolean", description, false)
    }
    pub fn required_integer(self, name: &str, description: &str) -> Self {
        self.typed(name, "integer", description, true)
    }
    pub fn required_string(self, name: &str, description: &str) -> Self {
        self.typed(name, "string", description, true)
    }
    pub fn string_enum(self, name: &str, description: &str, values: &[&str]) -> Self {
        self.add(name, json!({"type": "string", "description": description, "enum": values}), false)
    }
    pub fn integer_array(self, name: &str, description: &str) -> Self {
        self.add(
            name,
            json!({"type": "array", "items": {"type": "integer"}, "description": description}),
            false,
        )
    }

    pub fn add_string_array(self, name: &str, description: &str) -> Self {
        self.add(
            name,
            json!({"type": "array", "items": {"type": "string"}, "description": description}),
            false,
        )
    }

    pub fn build(self) -> Value {
        let mut schema = json!({"type": "object", "properties": self.properties});
        if !self.required.is_empty() {
            schema["required"] = Value::Array(self.required);
        }
        schema
    }
}

pub struct ToolRegistry<C> {
    tools: Vec<ToolDefinition<C>>,
}

impl<C> Default for ToolRegistry<C> {
    fn default() -> Self {
        Self { tools: Vec::new() }
    }
}

impl<C: 'static> ToolRegistry<C> {
    pub fn register<F, Fut>(&mut self, name: &str, description: &str, input_schema: Value, handler: F)
    where
        F: Fn(Rc<C>, Args) -> Fut + 'static,
        Fut: std::future::Future<Output = ToolResult> + 'static,
    {
        assert!(self.find(name).is_none(), "duplicate tool {name}");
        self.tools.push(ToolDefinition {
            name: name.to_owned(),
            description: description.to_owned(),
            input_schema,
            handler: Rc::new(move |context, args| Box::pin(handler(context, args))),
        });
    }

    pub fn find(&self, name: &str) -> Option<&ToolDefinition<C>> {
        self.tools.iter().find(|tool| tool.name == name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tools.iter().map(|tool| tool.name.as_str())
    }

    pub fn describe(&self) -> Value {
        Value::Array(
            self.tools
                .iter()
                .map(|tool| {
                    json!({
                        "name": tool.name,
                        "description": tool.description,
                        "inputSchema": tool.input_schema,
                    })
                })
                .collect(),
        )
    }
}

// Argument helpers -------------------------------------------------------------

pub fn number_arg(args: &Args, key: &str) -> Option<f64> {
    args.get(key).and_then(Value::as_f64).filter(|value| value.is_finite())
}

pub fn int_arg(args: &Args, key: &str) -> Option<i64> {
    number_arg(args, key).filter(|value| value.abs() <= i64::MAX as f64 / 2.0).map(|value| value as i64)
}

pub fn bool_arg(args: &Args, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

pub fn string_arg<'a>(args: &'a Args, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}
