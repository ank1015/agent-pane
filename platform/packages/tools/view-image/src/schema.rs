use crate::ViewImageOptions;
use serde_json::{Value, json};

#[must_use]
pub fn input_schema() -> Value {
    input_schema_with_options(ViewImageOptions::default())
}

#[must_use]
pub fn input_schema_with_options(options: ViewImageOptions) -> Value {
    let mut schema = json!({
        "type": "object", "properties": {
            "path": {"type": "string", "description": "Local filesystem path to an image file."}
        }, "required": ["path"], "additionalProperties": false
    });
    if options.can_request_original_detail && !options.unified_image_budget {
        schema["properties"]["detail"] = json!({
            "type": "string", "enum": ["high", "original"],
            "description": "Image detail level. Defaults to `high`; use `original` to preserve exact resolution."
        });
    }
    schema
}

#[must_use]
pub fn output_schema() -> Value {
    output_schema_with_options(ViewImageOptions::default())
}

#[must_use]
pub fn output_schema_with_options(options: ViewImageOptions) -> Value {
    let mut schema = json!({
        "type": "object", "properties": {
            "image_url": {"type": "string", "description": "Data URL for the loaded image."}
        }, "required": ["image_url"], "additionalProperties": false
    });
    if !options.unified_image_budget {
        schema["properties"]["detail"] = json!({
            "type": "string", "enum": ["high", "original"],
            "description": "Image detail hint returned by view_image. Returns `high` for default resized behavior or `original` when original resolution is preserved."
        });
        schema["required"] = json!(["image_url", "detail"]);
    }
    schema
}
