//! Floating AI design prompt and the optional Google Stitch provider adapter.
//!
//! Credentials are read only from `STITCH_API_KEY` when the user submits. The provider runs on a
//! worker thread; its editable export is reduced to the bounded provider-neutral engine IR before
//! it comes back to the UI thread.

use egui::{Color32, CornerRadius, Id, Rect, Stroke, pos2, vec2};
#[cfg(not(target_arch = "wasm32"))]
use serde_json::{Value, json};

use crate::FigApp;
use crate::i18n::tr;
use crate::theme::{Tokens, regular};

const MAX_PROMPT_CHARS: usize = 4_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Device {
    Mobile,
    #[default]
    Desktop,
    Tablet,
}

impl Device {
    #[cfg(not(target_arch = "wasm32"))]
    fn provider(self) -> &'static str {
        match self {
            Self::Mobile => "MOBILE",
            Self::Desktop => "DESKTOP",
            Self::Tablet => "TABLET",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Mobile => tr("Mobile"),
            Self::Desktop => tr("Desktop"),
            Self::Tablet => tr("Tablet"),
        }
    }
}

pub struct State {
    pub open: bool,
    pub prompt: String,
    pub device: Device,
    armed: bool,
    #[cfg(not(target_arch = "wasm32"))]
    job: Option<Job>,
}

#[cfg(not(target_arch = "wasm32"))]
struct Job {
    rx: std::sync::mpsc::Receiver<std::result::Result<Generated, String>>,
    document_uid: u64,
}

#[cfg(not(target_arch = "wasm32"))]
struct Generated {
    ir: Value,
    project_id: String,
    warnings: Vec<String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            open: false,
            prompt: String::new(),
            device: Device::Desktop,
            armed: false,
            #[cfg(not(target_arch = "wasm32"))]
            job: None,
        }
    }
}

impl State {
    fn busy(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.job.is_some()
        }
        #[cfg(target_arch = "wasm32")]
        {
            false
        }
    }
}

pub fn toggle(app: &mut FigApp) {
    app.ui.ai.open = !app.ui.ai.open;
    app.ui.ai.armed = app.ui.ai.open;
    if app.ui.ai.open {
        crate::menus::set_palette(&mut app.ui, false);
    }
}

fn start(app: &mut FigApp, ctx: &egui::Context) {
    let prompt = app.ui.ai.prompt.trim().to_string();
    if prompt.is_empty() {
        app.toast(tr("Describe the design you want to generate"));
        return;
    }
    if prompt.chars().count() > MAX_PROMPT_CHARS {
        app.toast(tr("The AI design prompt is too long"));
        return;
    }
    if app.session.prefs.ai_provider != "stitch" {
        app.toast(tr("Choose Google Stitch in Preferences before generating"));
        app.ui.preferences = true;
        return;
    }

    #[cfg(target_arch = "wasm32")]
    {
        let _ = ctx;
        app.toast(tr("Google Stitch generation is not available in the browser build yet"));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(api_key) = std::env::var("STITCH_API_KEY").ok().filter(|s| !s.trim().is_empty()) else {
            app.toast(tr("Set STITCH_API_KEY in the environment, then restart FigCraft"));
            app.ui.preferences = true;
            return;
        };
        let project_id = app.session.prefs.stitch_project_id.trim().to_string();
        let device = app.ui.ai.device;
        let Some(document_uid) = app.session.ds().ok().map(|d| d.uid) else {
            app.toast(tr("Open a design file before generating"));
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let wake = ctx.clone();
        std::thread::spawn(move || {
            let result = native::generate(&api_key, &project_id, &prompt, device);
            let _ = tx.send(result);
            wake.request_repaint();
        });
        app.ui.ai.job = Some(Job { rx, document_uid });
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn finish(app: &mut FigApp) {
    use std::sync::mpsc::TryRecvError;

    let result = match app.ui.ai.job.as_ref().map(|j| j.rx.try_recv()) {
        Some(Ok(result)) => result,
        Some(Err(TryRecvError::Empty)) | None => return,
        Some(Err(TryRecvError::Disconnected)) => Err(tr("The AI provider worker stopped before returning a result").to_string()),
    };
    let Some(job) = app.ui.ai.job.take() else { return };
    match result {
        Ok(mut generated) => {
            if app.session.ds().ok().map(|d| d.uid) != Some(job.document_uid) {
                app.toast(tr("The generated design was not inserted because its original file is no longer open"));
                return;
            }
            if app.session.prefs.stitch_project_id != generated.project_id {
                app.run("pref.stitchProject", json!({"projectId": generated.project_id}));
                app.services.save_prefs(&app.session.prefs);
            }
            if let Some(object) = generated.ir.as_object_mut()
                && let Ok(ds) = app.session.ds()
            {
                let width = object.get("width").and_then(Value::as_f64).unwrap_or(390.0);
                let height = object.get("height").and_then(Value::as_f64).unwrap_or(844.0);
                object.insert("x".into(), json!((ds.view.cx - width / 2.0).round()));
                object.insert("y".into(), json!((ds.view.cy - height / 2.0).round()));
            }
            if app.run("ai.applyDesign", generated.ir).is_some() {
                app.ui.ai.open = false;
                let omitted = generated.warnings.len();
                if omitted == 0 {
                    app.toast(tr("Generated an editable design with Google Stitch"));
                } else {
                    app.toast(format!("{} ({omitted} {})", tr("Generated an editable design"), tr("conversion notes")));
                }
            }
        }
        Err(error) => app.show_error(error),
    }
}

/// Draw the prompt bar above the toolbar and poll its provider worker without blocking a frame.
pub fn show(app: &mut FigApp, ctx: &egui::Context) {
    #[cfg(not(target_arch = "wasm32"))]
    finish(app);
    if !app.ui.ai.open {
        return;
    }
    let t = Tokens::get(ctx);
    let bar_w = crate::toolbar::main_width().max(620.0);
    let toolbar = crate::toolbar::pill_rect(app.canvas.rect, crate::toolbar::main_width());
    let height = 82.0;
    let area = Rect::from_min_size(pos2(toolbar.center().x - bar_w / 2.0, toolbar.min.y - height - 8.0), vec2(bar_w, height));
    let busy = app.ui.ai.busy();
    let pressed = ctx.input(|i| i.pointer.any_pressed());
    let mut close = false;
    if !busy && pressed && !std::mem::replace(&mut app.ui.ai.armed, false) {
        let on_toggle = app.ui.ai_rect.is_some_and(|b| ctx.input(|i| i.pointer.hover_pos().is_some_and(|p| b.contains(p))));
        if !on_toggle && ctx.input(|i| i.pointer.hover_pos().is_some_and(|p| !area.contains(p))) {
            close = true;
        }
    } else if !pressed {
        app.ui.ai.armed = false;
    }

    let mut submit = false;
    egui::Area::new(Id::new("ai_prompt")).fixed_pos(area.min).order(egui::Order::Foreground).show(ctx, |ui| {
        egui::Frame::NONE
            .fill(t.panel)
            .corner_radius(CornerRadius::same(13))
            .stroke(Stroke::new(1.0, t.border))
            .shadow(egui::Shadow { offset: [0, 8], blur: 32, spread: 0, color: t.shadow })
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                ui.set_width(bar_w - 16.0);
                ui.set_height(height - 16.0);
                ui.horizontal(|ui| {
                    let width = (ui.available_width() - 116.0).max(200.0);
                    let response = ui.add_enabled(
                        !busy,
                        egui::TextEdit::singleline(&mut app.ui.ai.prompt)
                            .hint_text(tr("Describe a screen to generate…"))
                            .desired_width(width)
                            .font(regular(14.0))
                            .frame(egui::Frame::NONE.fill(t.field).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::same(9))),
                    );
                    if !busy {
                        response.request_focus();
                    }
                    submit = response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let label = if busy { tr("Generating…") } else { tr("Generate") };
                    if ui.add_enabled(!busy && !app.ui.ai.prompt.trim().is_empty(), egui::Button::new(label).min_size(vec2(104.0, 36.0))).clicked() {
                        submit = true;
                    }
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(tr("Device")).size(11.0).color(t.text_secondary));
                    for device in [Device::Mobile, Device::Desktop, Device::Tablet] {
                        let selected = app.ui.ai.device == device;
                        let button =
                            egui::Button::new(egui::RichText::new(device.label()).size(11.0).color(if selected { t.text } else { t.text_secondary }))
                                .fill(if selected { t.hover } else { Color32::TRANSPARENT })
                                .corner_radius(CornerRadius::same(6));
                        if ui.add_enabled(!busy, button).clicked() {
                            app.ui.ai.device = device;
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(tr("Prompt is sent to Google Stitch")).size(10.0).color(t.text_secondary));
                    });
                });
            });
    });
    if !busy && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        close = true;
    }
    if close {
        app.ui.ai.open = false;
    } else if submit {
        start(app, ctx);
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::io::Read;
    use std::time::Duration;

    use reqwest::blocking::{Client, Response};
    use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
    use serde_json::{Value, json};

    use super::{Device, Generated};

    const ENDPOINT: &str = "https://stitch.googleapis.com/mcp";
    const MAX_MCP_BYTES: u64 = 16 * 1024 * 1024;
    const MAX_EXPORT_BYTES: u64 = 64 * 1024 * 1024;

    struct Mcp {
        client: Client,
        api_key: String,
        session: Option<String>,
        next_id: u64,
    }

    impl Mcp {
        fn new(api_key: &str) -> std::result::Result<Self, String> {
            let client = Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(310))
                // The fixed MCP endpoint must not redirect: following one could forward the API
                // key to a different origin. Signed export downloads carry no credential.
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| "Could not initialise the Google Stitch connection".to_string())?;
            Ok(Self { client, api_key: api_key.to_string(), session: None, next_id: 1 })
        }

        fn headers(&self) -> std::result::Result<HeaderMap, String> {
            let mut headers = HeaderMap::new();
            headers.insert(ACCEPT, HeaderValue::from_static("application/json, text/event-stream"));
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            headers.insert(
                HeaderName::from_static("x-goog-api-key"),
                HeaderValue::from_str(&self.api_key).map_err(|_| "STITCH_API_KEY contains invalid header characters".to_string())?,
            );
            if let Some(session) = &self.session {
                headers.insert(
                    HeaderName::from_static("mcp-session-id"),
                    HeaderValue::from_str(session).map_err(|_| "Google Stitch returned an invalid session id".to_string())?,
                );
                headers.insert(HeaderName::from_static("mcp-protocol-version"), HeaderValue::from_static("2025-06-18"));
            }
            Ok(headers)
        }

        fn post(&mut self, body: &Value, expect_body: bool) -> std::result::Result<Option<Value>, String> {
            let response = self.client.post(ENDPOINT).headers(self.headers()?).json(body).send().map_err(|_| {
                "Google Stitch could not be reached. A submitted generation may still finish remotely; FigCraft did not retry it.".to_string()
            })?;
            if let Some(session) = response.headers().get("mcp-session-id").and_then(|h| h.to_str().ok()) {
                self.session = Some(session.to_string());
            }
            let status = response.status();
            if !status.is_success() {
                return Err(match status.as_u16() {
                    401 | 403 => "Google Stitch rejected the credentials or project access".to_string(),
                    429 => "Google Stitch rate limit reached; wait before trying again".to_string(),
                    code => format!("Google Stitch returned HTTP {code}"),
                });
            }
            if !expect_body || status.as_u16() == 202 || response.content_length() == Some(0) {
                return Ok(None);
            }
            let content_type = response.headers().get(CONTENT_TYPE).and_then(|h| h.to_str().ok()).unwrap_or("").to_string();
            let text = limited_text(response, MAX_MCP_BYTES)?;
            if text.trim().is_empty() {
                return Ok(None);
            }
            let value = if content_type.starts_with("text/event-stream") {
                parse_sse(&text)?
            } else {
                serde_json::from_str(&text).map_err(|_| "Google Stitch returned invalid JSON".to_string())?
            };
            if let Some(error) = value.get("error") {
                let message = error.get("message").and_then(Value::as_str).unwrap_or("tool call failed");
                return Err(format!("Google Stitch: {message}"));
            }
            Ok(Some(value))
        }

        fn connect(&mut self) -> std::result::Result<(), String> {
            let id = self.next();
            self.post(
                &json!({"jsonrpc":"2.0","id":id,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"FigCraft","version":env!("CARGO_PKG_VERSION")}}}),
                true,
            )?
            .ok_or_else(|| "Google Stitch did not complete MCP initialization".to_string())?;
            let _ = self.post(&json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}), false)?;
            Ok(())
        }

        fn next(&mut self) -> u64 {
            let id = self.next_id;
            self.next_id = self.next_id.saturating_add(1);
            id
        }

        fn call(&mut self, name: &str, arguments: Value) -> std::result::Result<Value, String> {
            let id = self.next();
            let response = self
                .post(&json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}}), true)?
                .ok_or_else(|| format!("Google Stitch returned no result for {name}"))?;
            parse_tool_result(response.get("result").cloned().unwrap_or(Value::Null), name)
        }
    }

    fn limited_text(response: Response, max: u64) -> std::result::Result<String, String> {
        if response.content_length().is_some_and(|n| n > max) {
            return Err("Google Stitch response was larger than FigCraft's safety limit".to_string());
        }
        let mut bytes = Vec::new();
        response.take(max + 1).read_to_end(&mut bytes).map_err(|_| "Could not read the Google Stitch response".to_string())?;
        if bytes.len() as u64 > max {
            return Err("Google Stitch response was larger than FigCraft's safety limit".to_string());
        }
        String::from_utf8(bytes).map_err(|_| "Google Stitch returned non-UTF-8 data".to_string())
    }

    fn parse_sse(text: &str) -> std::result::Result<Value, String> {
        text.lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim)
            .filter(|line| *line != "[DONE]")
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .next_back()
            .ok_or_else(|| "Google Stitch returned an empty event stream".to_string())
    }

    fn parse_tool_result(result: Value, name: &str) -> std::result::Result<Value, String> {
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            let message = result
                .get("content")
                .and_then(Value::as_array)
                .and_then(|a| a.iter().find_map(|c| c.get("text").and_then(Value::as_str)))
                .unwrap_or("tool call failed");
            return Err(format!("Google Stitch {name}: {message}"));
        }
        if let Some(value) = result.get("structuredContent") {
            return Ok(value.clone());
        }
        if let Some(text) = result.get("content").and_then(Value::as_array).and_then(|a| a.iter().find_map(|c| c.get("text").and_then(Value::as_str)))
        {
            return serde_json::from_str(text).or_else(|_| Ok(json!({"text": text})));
        }
        Ok(result)
    }

    fn project_id(value: &Value) -> Option<String> {
        value
            .get("projectId")
            .or_else(|| value.get("id"))
            .and_then(Value::as_str)
            .or_else(|| value.get("name").and_then(Value::as_str).and_then(|n| n.strip_prefix("projects/")))
            .map(str::to_string)
    }

    fn first_screen(value: &Value) -> Option<&Value> {
        if value.get("name").and_then(Value::as_str).is_some_and(|n| n.contains("/screens/")) {
            return Some(value);
        }
        match value {
            Value::Array(items) => items.iter().find_map(first_screen),
            Value::Object(map) => map.values().find_map(first_screen),
            _ => None,
        }
    }

    fn export_url(screen: &Value) -> Option<&str> {
        screen.get("figmaExport").and_then(|f| f.get("downloadUrl")).and_then(Value::as_str)
    }

    fn download_export(url: &str) -> std::result::Result<Vec<u8>, String> {
        if !url.starts_with("https://") {
            return Err("Google Stitch returned a non-HTTPS editable export URL".to_string());
        }
        // Export URLs are signed rather than authenticated with the Stitch API key. Permit a
        // small number of redirects, but never leave HTTPS.
        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 {
                    attempt.error("too many redirects")
                } else if attempt.url().scheme() != "https" {
                    attempt.error("non-HTTPS redirect")
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .map_err(|_| "Could not prepare the Google Stitch editable export download".to_string())?;
        let response = client.get(url).send().map_err(|_| "Could not download the Google Stitch editable export".to_string())?;
        if response.url().scheme() != "https" {
            return Err("Google Stitch redirected the editable export away from HTTPS".to_string());
        }
        if !response.status().is_success() {
            return Err(format!("Google Stitch editable export returned HTTP {}", response.status().as_u16()));
        }
        if response.content_length().is_some_and(|n| n > MAX_EXPORT_BYTES) {
            return Err("Google Stitch editable export was larger than FigCraft's 64 MiB limit".to_string());
        }
        let mut bytes = Vec::new();
        response.take(MAX_EXPORT_BYTES + 1).read_to_end(&mut bytes).map_err(|_| "Could not read the Google Stitch editable export".to_string())?;
        if bytes.len() as u64 > MAX_EXPORT_BYTES {
            return Err("Google Stitch editable export was larger than FigCraft's 64 MiB limit".to_string());
        }
        Ok(bytes)
    }

    pub(super) fn generate(api_key: &str, configured_project: &str, prompt: &str, device: Device) -> std::result::Result<Generated, String> {
        let mut mcp = Mcp::new(api_key)?;
        mcp.connect()?;
        let project = if configured_project.is_empty() {
            let created = mcp.call("create_project", json!({"title":"FigCraft AI designs"}))?;
            project_id(&created).ok_or_else(|| "Google Stitch created a project without returning its id".to_string())?
        } else {
            configured_project.to_string()
        };
        // Never retry this mutating call: a lost response may still represent a successful remote generation.
        let generated = mcp.call("generate_screen_from_text", json!({"projectId":project,"prompt":prompt,"deviceType":device.provider()}))?;
        let screen = first_screen(&generated).ok_or_else(|| {
            generated
                .get("text")
                .and_then(Value::as_str)
                .map_or_else(|| "Google Stitch returned no generated screen".to_string(), |s| format!("Google Stitch needs more detail: {s}"))
        })?;
        let title = screen.get("title").and_then(Value::as_str).unwrap_or("AI design").to_string();
        let url = export_url(screen).ok_or_else(|| {
            "Google Stitch returned HTML but no editable export, so FigCraft left the canvas unchanged rather than inserting a flattened screenshot".to_string()
        })?;
        let bytes = download_export(url)?;
        let (doc, mut warnings) = figcraft_format::fig::import(&bytes, &format!("{title}.fig"))
            .map_err(|e| format!("Could not read Google Stitch's editable export: {e}"))?;
        let converted = figcraft_engine::ai::document_to_ir(&doc, &title).map_err(|e| e.to_string())?;
        warnings.extend(converted.warnings);
        Ok(Generated { ir: converted.ir, project_id: project, warnings })
    }

    #[cfg(test)]
    mod tests {
        use serde_json::json;

        #[test]
        fn tool_envelopes_prefer_structured_content() {
            let value = super::parse_tool_result(
                json!({"structuredContent":{"projectId":"p1"},"content":[{"type":"text","text":"{\"projectId\":\"wrong\"}"}]}),
                "create_project",
            )
            .unwrap();
            assert_eq!(value["projectId"], "p1");
        }

        #[test]
        fn finds_nested_generated_screen_and_editable_export() {
            let value = json!({"outputComponents":[{"design":{"screens":[{"name":"projects/p/screens/s","title":"Home","figmaExport":{"downloadUrl":"https://files.example/s.fig"}}]}}]});
            let screen = super::first_screen(&value).unwrap();
            assert_eq!(super::export_url(screen), Some("https://files.example/s.fig"));
        }

        #[test]
        fn event_stream_uses_last_json_message() {
            let value = super::parse_sse("event: message\ndata: {\"jsonrpc\":\"2.0\",\"result\":{\"ok\":true}}\n\n").unwrap();
            assert_eq!(value["result"]["ok"], true);
        }
    }
}
