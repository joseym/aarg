//! Reading a document the deterministic text path can't: a photo, a
//! screenshot, or a scanned PDF with no text layer. The model transcribes
//! it to plain text, which then flows through the same ingest / JD-parse
//! pipeline as pasted text — so nothing downstream knows the difference.
//!
//! Honesty note: unlike `pdf_extract` (deterministic), this is
//! model-generated text. The prompt is strict — transcribe verbatim, invent
//! nothing, mark anything unreadable rather than guess — and the result is
//! still structured by the same parser and reviewed by the user via
//! `dataset show` / `validate` before it is ever tailored. A misread is a
//! catchable data-entry error, not a tailoring fabrication.

use crate::agent::{AgentContext, ModelTier};
use crate::llm::{Attachment, CompletionRequest, LlmError, Message};

/// What the model is asked to do with the attached document. Strict on
/// purpose: the never-fabricate posture starts here.
const TRANSCRIBE_PROMPT: &str = "\
Transcribe this document to plain text exactly as it appears. Preserve the \
wording, section headings, dates, and bullet points. Do not summarize, \
rephrase, correct, translate, or add anything that is not in the document. \
If part of it is unreadable, write [unreadable] in that spot rather than \
guessing. Output only the transcribed text, with no preamble or commentary.";

/// Headroom for a transcribed resume or job-description page. A page of
/// prose is well under this; the cap only guards a runaway response.
const MAX_TOKENS: u32 = 8000;

/// Transcribe an attached image or PDF to plain text using the cheap tier
/// (Haiku, which supports vision). Returns the model's text verbatim for the
/// caller to feed into the normal parser.
pub async fn transcribe(
    ctx: &AgentContext<'_>,
    attachment: Attachment,
) -> Result<String, LlmError> {
    let model = ctx.model.resolve("vision_transcribe_v1", ModelTier::Cheap);
    let request = CompletionRequest {
        model: model.to_string(),
        max_tokens: MAX_TOKENS,
        system: None,
        messages: vec![Message::user_with_attachment(TRANSCRIBE_PROMPT, attachment)],
        temperature: None,
        tools: Vec::new(),
    };
    let response = ctx.llm.complete(request).await?;
    Ok(response.text)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::llm::MockLlmClient;
    use crate::trace::Tracer;

    #[tokio::test]
    async fn transcribe_sends_the_attachment_and_returns_the_model_text() {
        let client = MockLlmClient::new();
        client.enqueue("Sam Rivera\nStaff Engineer");
        let ctx = AgentContext {
            llm: &client,
            model: &"claude-haiku-4-5",
            tracer: &Tracer::DISABLED,
            sink: None,
        };

        let text = transcribe(
            &ctx,
            Attachment::Image {
                media_type: "image/png".into(),
                data: "aGVsbG8=".into(),
            },
        )
        .await
        .unwrap();

        assert_eq!(text, "Sam Rivera\nStaff Engineer");
        // The request carried the attachment, not a bare text turn.
        let sent = client.requests();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].messages[0].attachments.len(), 1);
    }
}
