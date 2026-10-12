//! The rendered statement: a finalized statement as a PDF the organization can
//! download (issue #205).
//!
//! The credentials-and-credit decision shipped "Lago-lite statements" as data
//! and deferred PDF invoicing — this is the rendered half. Rendering is pure
//! Rust and stays inside the single binary: `printpdf` draws the page with the
//! standard-14 builtin fonts (no font bytes ship), and the QR pointing back at
//! `/verify` is drawn as vector rectangles, so no image codec is pulled in. A
//! statement only exports once it is finalized — the document reads the stored
//! lines, so the PDF can never disagree with the API's own answer.
//!
//! The page is deliberately plain: a title and issuer, the statement meta
//! (period, status, due date), the itemized lines, the totals, and a footer
//! carrying the ledger window the lines prove plus a QR to `/verify` — the
//! paper pointing back at the checkable record.

use printpdf::{
    BuiltinFont, Color, LinePoint, Mm, Op, PaintMode, PdfDocument, PdfFontHandle, PdfPage,
    PdfSaveOptions, Point, Polygon, PolygonRing, Pt, Rgb, TextItem, WindingOrder,
};

use crate::error::WalletError;
use crate::statements::{Statement, StatementLine};

/// A4, the page the statement renders on.
const PAGE_W: Mm = Mm(210.0);
const PAGE_H: Mm = Mm(297.0);
const MARGIN: f32 = 18.0;
const BODY: f32 = 9.0;
const SMALL: f32 = 8.0;
const TABLE_TOP: f32 = 205.0;
const ROW_H: f32 = 6.0;
/// The totals block's first line sits this far below the last table row.
const FOOTER_Y: f32 = 40.0;
/// The QR module edge length in millimetres.
const QR_MODULE: f32 = 1.5;

/// Column x-origins, millimetres from the left edge: channel, model, turns,
/// input, output, cached, reasoning, amount (right-aligned).
const COL: [f32; 8] = [18.0, 58.0, 96.0, 116.0, 136.0, 156.0, 176.0, 192.0];

/// The money the dashboard's `credits()` renders — minor units as credits,
/// six decimals, integer arithmetic so no float ever touches money.
fn credits(minor: i64) -> String {
    let sign = if minor < 0 { "-" } else { "" };
    let abs = minor.unsigned_abs();
    format!("{sign}{}.{:06}", abs / 1_000_000, abs % 1_000_000)
}

/// One text run on the page: `StartTextSection` … `EndTextSection` with the
/// cursor set once, so every label lands where it was placed rather than
/// flowing from the last.
fn text(ops: &mut Vec<Op>, x: f32, y: f32, size: f32, bold: bool, content: &str) {
    ops.extend_from_slice(&[
        Op::StartTextSection,
        Op::SetTextCursor {
            pos: Point::new(Mm(x), Mm(y)),
        },
        Op::SetFont {
            font: PdfFontHandle::Builtin(if bold {
                BuiltinFont::HelveticaBold
            } else {
                BuiltinFont::Helvetica
            }),
            size: Pt(size),
        },
        Op::ShowText {
            items: vec![TextItem::Text(content.to_owned())],
        },
        Op::EndTextSection,
    ]);
}

/// Right-aligned text: the cursor backs off by an approximate advance so a
/// column of amounts lines up on its right edge. Helvetica's average glyph is
/// about 0.5 em, which is close enough for a statement's totals column.
fn text_right(ops: &mut Vec<Op>, right_x: f32, y: f32, size: f32, bold: bool, content: &str) {
    let approx = content.len() as f32 * size * 0.28;
    text(ops, right_x - approx, y, size, bold, content);
}

/// A filled axis-aligned rectangle: a closed quadrilateral `Polygon` in
/// `PaintMode::Fill`. The QR modules and the table rules are all drawn this way.
fn rect(ops: &mut Vec<Op>, x: f32, y: f32, w: f32, h: f32) {
    let ring = PolygonRing {
        points: [(x, y), (x + w, y), (x + w, y + h), (x, y + h)]
            .into_iter()
            .map(|(px, py)| LinePoint {
                p: Point::new(Mm(px), Mm(py)),
                bezier: false,
            })
            .collect(),
    };
    ops.push(Op::DrawPolygon {
        polygon: Polygon {
            rings: vec![ring],
            mode: PaintMode::Fill,
            winding_order: WindingOrder::NonZero,
        },
    });
}

/// A thin filled rectangle standing in for a stroked line: one op, no dash or
/// cap handling to set up.
fn rule(ops: &mut Vec<Op>, x: f32, y: f32, w: f32, thickness: f32) {
    rect(ops, x, y, w, thickness);
}

/// The `/verify` QR, drawn module-by-module as filled squares — vector rects,
/// so no image codec. Rendered only when a URL exists; `qrcode`'s `Code` gives
/// the dark modules over `width()`.
fn qr(ops: &mut Vec<Op>, url: &str, x: f32, y: f32) -> Result<(), WalletError> {
    let code = qrcode::QrCode::new(url.as_bytes())
        .map_err(|error| WalletError::InvalidInput(format!("the verify URL QR: {error}")))?;
    let modules = code.to_colors();
    let width = code.width() as i32;
    ops.push(Op::SetFillColor {
        col: Color::Rgb(Rgb::new(0.0, 0.0, 0.0, None)),
    });
    for (i, module) in modules.iter().enumerate() {
        if *module != qrcode::Color::Dark {
            continue;
        }
        let col = i as i32 % width;
        // The PDF origin is bottom-left; the QR grid is row-major from the
        // top, so the row index flips onto y.
        let row = width - 1 - (i as i32 / width);
        rect(
            ops,
            x + col as f32 * QR_MODULE,
            y + row as f32 * QR_MODULE,
            QR_MODULE,
            QR_MODULE,
        );
    }
    Ok(())
}

/// Renders a finalized statement as PDF bytes.
///
/// `verify_url` is the base the QR and the footer print — the deployment's
/// `OXSUM_PUBLIC_URL` plus `/verify` when named, else the relative `/verify`
/// path. Only a finalized statement renders; the callers refuse drafts before
/// reaching here, and the function asserts the same so the bytes can never
/// describe a working copy.
///
/// # Errors
///
/// Refuses a non-finalized statement, and reports a QR the URL cannot form.
pub fn render_statement_pdf(
    statement: &Statement,
    organization: &str,
    lines: &[StatementLine],
    verify_url: Option<&str>,
) -> Result<Vec<u8>, WalletError> {
    use crate::statements::StatementStatus;
    if statement.status != StatementStatus::Finalized {
        return Err(WalletError::InvalidInput(
            "a statement exports only once finalized".into(),
        ));
    }
    let mut doc = PdfDocument::new(&format!("Statement {}", statement.period));
    let black = Color::Rgb(Rgb::new(0.0, 0.0, 0.0, None));
    let grey = Color::Rgb(Rgb::new(0.35, 0.35, 0.35, None));
    let mut ops = Vec::new();
    ops.push(Op::SetFillColor { col: black.clone() });

    // ── header ────────────────────────────────────────────────────────────
    text(&mut ops, MARGIN, 272.0, 22.0, true, "oxsum");
    text(
        &mut ops,
        MARGIN,
        262.0,
        BODY,
        false,
        "AI credit wallet — usage statement",
    );
    ops.push(Op::SetFillColor { col: grey.clone() });
    text(
        &mut ops,
        MARGIN,
        254.0,
        SMALL,
        false,
        "Verifiable statement",
    );
    ops.push(Op::SetFillColor { col: black.clone() });

    // Right-aligned meta block.
    let due = statement
        .due_date
        .map(|d| d.to_string())
        .unwrap_or_else(|| "—".to_owned());
    for (i, line) in [
        format!("Period       {}", statement.period),
        format!("Status       {}", statement.status.as_str()),
        format!("Payment      {}", statement.payment_status.as_str()),
        format!("Due          {due}"),
    ]
    .iter()
    .enumerate()
    {
        text_right(
            &mut ops,
            PAGE_W.0 - MARGIN,
            270.0 - i as f32 * 5.0,
            BODY,
            false,
            line,
        );
    }
    rule(&mut ops, MARGIN, 246.0, PAGE_W.0 - 2.0 * MARGIN, 0.3);

    // ── billed-to ─────────────────────────────────────────────────────────
    text(&mut ops, MARGIN, 236.0, SMALL, false, "Billed to");
    text(&mut ops, MARGIN, 230.0, 11.0, true, organization);

    // ── the lines table ───────────────────────────────────────────────────
    let headers = [
        "Channel",
        "Model",
        "Turns",
        "Input",
        "Output",
        "Cached",
        "Reasoning",
        "Amount",
    ];
    ops.push(Op::SetFillColor { col: grey.clone() });
    for (x, header) in COL.iter().zip(headers.iter()) {
        text(&mut ops, *x, TABLE_TOP, SMALL, true, header);
    }
    ops.push(Op::SetFillColor { col: black.clone() });
    rule(
        &mut ops,
        MARGIN,
        TABLE_TOP - 1.5,
        PAGE_W.0 - 2.0 * MARGIN,
        0.3,
    );

    let mut y = TABLE_TOP - ROW_H;
    for line in lines {
        let cells = [
            line.channel.clone(),
            line.model.clone(),
            line.turns.to_string(),
            line.input_tokens.to_string(),
            line.output_tokens.to_string(),
            line.cached_tokens.to_string(),
            line.reasoning_tokens.to_string(),
            credits(line.amount_minor),
        ];
        for (x, cell) in COL.iter().zip(cells.iter()) {
            text(&mut ops, *x, y, BODY, false, cell);
        }
        y -= ROW_H;
    }
    rule(
        &mut ops,
        MARGIN,
        y + ROW_H - 1.5,
        PAGE_W.0 - 2.0 * MARGIN,
        0.3,
    );
    y -= ROW_H;

    // ── totals ────────────────────────────────────────────────────────────
    let totals = [
        ("Total", statement.total_minor),
        ("Credit drawn", statement.credit_drawn_minor),
        ("Paid", statement.paid_minor),
        ("Outstanding", statement.outstanding_minor),
    ];
    for (label, amount) in totals {
        text(&mut ops, COL[0], y, BODY, false, label);
        text_right(
            &mut ops,
            COL[7],
            y,
            BODY,
            label == "Total",
            &credits(amount),
        );
        y -= ROW_H;
    }

    // ── the proof window + QR ─────────────────────────────────────────────
    ops.push(Op::SetFillColor { col: grey });
    let window = match (statement.log_from_index, statement.log_to_index) {
        (Some(from), Some(to)) => format!("log entries {from}–{to}"),
        _ => "log window unpinned".to_owned(),
    };
    let verify = verify_url.unwrap_or("/verify");
    text(
        &mut ops,
        MARGIN,
        FOOTER_Y,
        SMALL,
        false,
        &format!(
            "This statement is anchored to the ledger: {window}. Verify the \
             underlying records at {verify}"
        ),
    );
    if verify_url.is_some() {
        qr(&mut ops, verify, PAGE_W.0 - MARGIN - 32.0, FOOTER_Y - 30.0)?;
    }

    let page = PdfPage::new(PAGE_W, PAGE_H, ops);
    Ok(doc
        .with_pages(vec![page])
        .save(&PdfSaveOptions::default(), &mut Vec::new()))
}

#[cfg(test)]
mod tests {
    // The tests may expect: a panic here is a failing test, which is what a test is for.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use time::macros::date;

    use super::*;
    use crate::statements::PaymentStatus;

    fn statement() -> Statement {
        Statement {
            id: uuid::Uuid::nil(),
            organization_id: uuid::Uuid::nil(),
            period: "2026-09".to_owned(),
            status: crate::statements::StatementStatus::Finalized,
            payment_status: PaymentStatus::Pending,
            total_minor: 7_000_000,
            credit_drawn_minor: 7_000_000,
            paid_minor: 0,
            outstanding_minor: 7_000_000,
            payment_terms_days: 14,
            due_date: Some(date!(2026 - 10 - 15)),
            log_from_index: Some(3),
            log_to_index: Some(8),
            entry_count: 2,
            finalized_at: None,
            paid_at: None,
            overdue_at: None,
            suspended_at: None,
            created_at: time::OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn lines() -> Vec<StatementLine> {
        vec![
            StatementLine {
                channel: "anthropic".to_owned(),
                model: "claude".to_owned(),
                turns: 1,
                input_tokens: 100,
                output_tokens: 50,
                cached_tokens: 0,
                reasoning_tokens: 0,
                amount_minor: 4_000_000,
            },
            StatementLine {
                channel: "openai".to_owned(),
                model: "gpt-4o".to_owned(),
                turns: 1,
                input_tokens: 100,
                output_tokens: 50,
                cached_tokens: 0,
                reasoning_tokens: 0,
                amount_minor: 3_000_000,
            },
        ]
    }

    /// The rendered bytes are a real PDF: magic, trailer, and a parse — and the
    /// text the parse reads back carries the statement's own numbers, so the
    /// export is the stored document, not a stub.
    #[test]
    fn the_rendered_pdf_parses_and_carries_the_statement() {
        let bytes = render_statement_pdf(
            &statement(),
            "Acme Corp",
            &lines(),
            Some("https://ox.example/verify"),
        )
        .expect("renders");
        assert!(bytes.starts_with(b"%PDF-"), "not a PDF header");
        assert!(bytes.windows(5).any(|w| w == b"%%EOF"), "no trailer");
        assert!(bytes.len() > 1_000, "a statement page is not a stub");

        let parsed = PdfDocument::parse(&bytes, &Default::default(), &mut Vec::new())
            .expect("the rendered bytes parse as a PDF");
        assert_eq!(parsed.pages.len(), 1);
        let page_text = parsed.extract_text().remove(0).join("\n");
        for expected in [
            "Acme Corp",
            "2026-09",
            "finalized",
            "2026-10-15",
            "anthropic",
            "gpt-4o",
            // 7_000_000 minor is 7.000000 credits — the dashboard's spelling.
            "7.000000",
            // The proof window the page anchors to.
            "log entries 3–8",
            "https://ox.example/verify",
        ] {
            assert!(
                page_text.contains(expected),
                "the page does not carry {expected:?}:\n{page_text}"
            );
        }
    }

    /// The export is finalized-only by construction, not just by the route's
    /// guard: a draft refuses to render, so the bytes can never describe a
    /// working copy.
    #[test]
    fn a_draft_refuses_to_render() {
        let mut draft = statement();
        draft.status = crate::statements::StatementStatus::Draft;
        assert!(render_statement_pdf(&draft, "Acme", &lines(), None).is_err());
    }
}
