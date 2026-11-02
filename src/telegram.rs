//! Minimal Telegram Bot API client (sendMessage / getUpdates over HTTPS).
//! A full bot framework such as teloxide is not needed to post into a group.
//! Adapted from GoldCatcher (session-bias-pro-GoldCatcher, same author).

use crate::config::TelegramConfig;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Clone)]
pub struct Telegram {
    http: reqwest::Client,
    cfg: TelegramConfig,
}

impl Telegram {
    pub fn new(cfg: &TelegramConfig) -> Result<Self> {
        if cfg.bot_token.trim().is_empty() {
            bail!("Telegram bot token missing: set TELEGRAM_BOT_TOKEN or telegram.bot_token");
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            http,
            cfg: cfg.clone(),
        })
    }

    fn url(&self, method: &str) -> String {
        format!(
            "{}/bot{}/{}",
            self.cfg.api_base.trim_end_matches('/'),
            self.cfg.bot_token.trim(),
            method
        )
    }

    async fn call(&self, method: &str, body: &Value) -> Result<Value> {
        let resp = self
            .http
            .post(self.url(method))
            .json(body)
            .send()
            .await
            .context("Telegram request")?;
        let v: Value = resp.json().await.context("Telegram response")?;
        if v.get("ok").and_then(Value::as_bool) == Some(true) {
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
        let code = v.get("error_code").and_then(Value::as_i64).unwrap_or(0);
        let desc = v
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        let retry = v.pointer("/parameters/retry_after").and_then(Value::as_u64);
        let migrate_to = v
            .pointer("/parameters/migrate_to_chat_id")
            .and_then(Value::as_i64);
        bail!(TgError {
            code,
            desc: desc.to_string(),
            retry_after: retry,
            migrate_to,
        })
    }

    pub fn chat_id(&self) -> &str {
        self.cfg.chat_id.trim()
    }

    /// Sends plain text to the configured chat, retrying on rate limits and
    /// network errors. Messages over Telegram's 4096-char limit are split.
    pub async fn send(&mut self, text: &str) -> Result<()> {
        if self.cfg.chat_id.trim().is_empty() {
            bail!(
                "Telegram chat id missing: set TELEGRAM_CHAT_ID or telegram.chat_id (run `goldib chat-id`)"
            );
        }
        for chunk in split(text, 4000) {
            let mut body = json!({
                "chat_id": self.cfg.chat_id.trim(),
                "text": chunk,
                "disable_web_page_preview": true,
                "disable_notification": self.cfg.disable_notification,
            });
            if let Some(t) = self.cfg.message_thread_id {
                body["message_thread_id"] = json!(t);
            }
            let mut attempt = 0u32;
            loop {
                match self.call("sendMessage", &body).await {
                    Ok(_) => break,
                    Err(e) => {
                        attempt += 1;
                        let tg = e.downcast_ref::<TgError>();
                        // The group became a supergroup and got a new id: follow it.
                        if let Some(new_id) = tg.and_then(|t| t.migrate_to) {
                            tracing::warn!(
                                "Telegram group was upgraded to a supergroup: chat id {} → {new_id}. \
                                 Update TELEGRAM_CHAT_ID={new_id} in your .env.",
                                self.chat_id()
                            );
                            self.cfg.chat_id = new_id.to_string();
                            body["chat_id"] = json!(self.cfg.chat_id);
                            continue;
                        }
                        // 400/401/403 are configuration errors – retrying will not help.
                        // Network errors are retried for ~15 minutes so a short outage
                        // (Wi-Fi drop, laptop waking up) does not lose a signal.
                        if is_config_error(&e) || attempt >= 20 {
                            return Err(e);
                        }
                        let wait = tg
                            .and_then(|t| t.retry_after)
                            .unwrap_or(2u64.pow(attempt.min(6)));
                        tracing::warn!("Telegram send failed ({e:#}); retry {attempt} in {wait}s");
                        tokio::time::sleep(Duration::from_secs(wait)).await;
                    }
                }
            }
        }
        Ok(())
    }

    pub async fn get_me(&self) -> Result<Value> {
        self.call("getMe", &json!({})).await
    }

    /// Read-only check that the bot can see the configured chat (posts nothing).
    pub async fn get_chat(&self) -> Result<Value> {
        self.call("getChat", &json!({ "chat_id": self.chat_id() }))
            .await
    }

    /// Chats the bot has seen recently (to find a group's chat id).
    pub async fn recent_chats(&self) -> Result<Vec<(i64, String, String)>> {
        let res = self
            .call(
                "getUpdates",
                &json!({ "timeout": 0, "allowed_updates": [] }),
            )
            .await?;
        let mut out: Vec<(i64, String, String)> = Vec::new();
        for u in res.as_array().into_iter().flatten() {
            for key in [
                "message",
                "channel_post",
                "my_chat_member",
                "edited_message",
            ] {
                if let Some(chat) = u.get(key).and_then(|m| m.get("chat")) {
                    let id = chat.get("id").and_then(Value::as_i64).unwrap_or(0);
                    let kind = chat
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let title = chat
                        .get("title")
                        .or_else(|| chat.get("username"))
                        .or_else(|| chat.get("first_name"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    if id != 0 && !out.iter().any(|c| c.0 == id) {
                        out.push((id, kind, title));
                    }
                }
            }
        }
        Ok(out)
    }
}

#[derive(Debug)]
struct TgError {
    code: i64,
    desc: String,
    retry_after: Option<u64>,
    migrate_to: Option<i64>,
}

/// Errors that retrying cannot fix: bad token, wrong chat id, bot removed from the group.
pub fn is_config_error(e: &anyhow::Error) -> bool {
    e.downcast_ref::<TgError>()
        .is_some_and(|t| t.migrate_to.is_none() && matches!(t.code, 400 | 401 | 403 | 404))
}

impl std::fmt::Display for TgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Telegram error {}: {}", self.code, self.desc)
    }
}

impl std::error::Error for TgError {}

fn split(text: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in text.lines() {
        if !cur.is_empty() && cur.chars().count() + line.chars().count() + 1 > max {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push('\n');
        }
        cur.push_str(line);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}
