use super::ContentItem;

fn tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

fn snippet(text: &str, budget: usize) -> String {
    let bytes = budget.saturating_mul(4);
    if text.len() <= bytes {
        return text.into();
    }
    let mut left = bytes.div_ceil(2);
    let mut right = text.len() - bytes / 2;
    while !text.is_char_boundary(left) {
        left -= 1;
    }
    while !text.is_char_boundary(right) {
        right += 1;
    }
    format!(
        "{}…{} tokens truncated…{}",
        &text[..left],
        (text.len() - bytes).div_ceil(4),
        &text[right..]
    )
}

pub(super) fn truncate(items: Vec<ContentItem>, mut budget: usize) -> Vec<ContentItem> {
    if items
        .iter()
        .all(|item| matches!(item, ContentItem::InputText { .. }))
    {
        let mut combined = String::new();
        for item in &items {
            if let ContentItem::InputText { text } = item {
                if !combined.is_empty() {
                    combined.push('\n');
                }
                combined.push_str(text);
            }
        }
        if tokens(&combined) <= budget {
            return items;
        }
        return vec![ContentItem::InputText {
            text: format!(
                "Warning: truncated output (original token count: {})\nTotal output lines: {}\n\n{}",
                tokens(&combined),
                combined.lines().count(),
                snippet(&combined, budget)
            ),
        }];
    }
    let mut output = Vec::new();
    let (mut omitted_text, mut omitted_audio) = (0, 0);
    for item in items {
        match item {
            ContentItem::InputText { text } if text.is_empty() => {}
            ContentItem::InputText { text } => {
                if budget == 0 {
                    omitted_text += 1;
                } else {
                    output.push(ContentItem::InputText {
                        text: snippet(&text, budget),
                    });
                    budget = budget.saturating_sub(tokens(&text));
                }
            }
            ContentItem::InputAudio { audio_url } => {
                // Conservative data-URL estimate; this runtime does not depend on
                // a media decoder or model-specific audio tokenization library.
                let cost = tokens(&audio_url);
                if cost > budget {
                    omitted_audio += 1;
                } else {
                    budget -= cost;
                    output.push(ContentItem::InputAudio { audio_url });
                }
            }
            image @ ContentItem::InputImage { .. } => output.push(image),
        }
    }
    for (count, kind) in [(omitted_text, "text"), (omitted_audio, "audio")] {
        if count > 0 {
            output.push(ContentItem::InputText {
                text: format!("[omitted {count} {kind} items ...]"),
            });
        }
    }
    output
}
