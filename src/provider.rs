use serde_json::{Value, json};
use std::{env, thread, time::Duration};

const ATTEMPTS: u32 = 3;

pub struct Judge<'a> {
    pub state: &'a str,
    pub question: &'a str,
    /// (yes, no) criteria.
    pub criteria: Option<(&'a str, &'a str)>,
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Provider {
    #[value(name = "systemone")]
    SystemOne,
    Cloudflare,
    #[value(name = "openai")]
    OpenAI,
}

impl Provider {
    /// Ask the provider the gate question. Returns (P(yes), model id).
    pub fn judge(
        self,
        j: &Judge,
        url: Option<&str>,
        model: Option<&str>,
        timeout: Duration,
    ) -> Result<(f64, String), String> {
        let (default_url, token, body) = self.request(j, model, url.is_some())?;
        let url = url.map(str::to_string).unwrap_or(default_url);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .build()
            .into();

        let mut last_err = String::new();
        for attempt in 0..ATTEMPTS {
            if attempt > 0 {
                thread::sleep(Duration::from_secs(1 << (attempt - 1)));
            }
            let res = agent
                .post(&url)
                .header("Authorization", &format!("Bearer {token}"))
                .send_json(&body);
            let mut resp = match res {
                Ok(r) => r,
                Err(e) => {
                    last_err = format!("request failed: {e}");
                    continue;
                }
            };
            let status = resp.status().as_u16();
            let text = resp.body_mut().read_to_string().unwrap_or_default();
            if (200..300).contains(&status) {
                let v: Value =
                    serde_json::from_str(&text).map_err(|e| format!("bad response JSON: {e}"))?;
                return parse(&v);
            }
            last_err = format!("HTTP {status}: {}", text.trim());
            if !(status == 429 || status >= 500) {
                break;
            }
        }
        Err(last_err)
    }

    /// Build (default URL, bearer token, JSON body) for this provider.
    fn request(
        self,
        j: &Judge,
        model: Option<&str>,
        has_url: bool,
    ) -> Result<(String, String, Value), String> {
        let state = j.state;
        let questions = questions(j);
        match self {
            Provider::SystemOne => {
                let model = model.unwrap_or("jev-latest");
                Ok((
                    "https://api.typesafe.ai/v1/systemone".into(),
                    var("JEV_API_KEY")?,
                    json!({"model": model, "state": state, "questions": questions}),
                ))
            }
            Provider::Cloudflare => {
                let model = model.unwrap_or("typesafe/jev");
                let url = if has_url {
                    String::new()
                } else {
                    let account = var("CLOUDFLARE_ACCOUNT_ID")?;
                    format!("https://api.cloudflare.com/client/v4/accounts/{account}/ai/run")
                };
                Ok((
                    url,
                    var("CLOUDFLARE_API_TOKEN")?,
                    json!({"model": model, "input": {"state": state, "questions": questions}}),
                ))
            }
            // ponytail: OpenAI Decisions request schema unverified (SPEC §4.3); implement once confirmed.
            Provider::OpenAI => Err("openai provider not yet implemented".into()),
        }
    }
}

fn questions(j: &Judge) -> Value {
    let mut q = json!({"type": "noul", "instructions": j.question});
    if let Some((yes, no)) = j.criteria {
        q["criteria"] = json!({"true": yes, "false": no});
    }
    json!({"gate": q})
}

fn var(name: &str) -> Result<String, String> {
    env::var(name).map_err(|_| format!("missing environment variable {name}"))
}

/// Extract (answers.gate.noul, model). Accepts a Cloudflare-style `result` envelope.
fn parse(v: &Value) -> Result<(f64, String), String> {
    let v = v.get("result").unwrap_or(v);
    let noul = v["answers"]["gate"]["noul"]
        .as_f64()
        .ok_or("response missing answers.gate.noul")?;
    if !(0.0..=1.0).contains(&noul) {
        return Err(format!("noul out of range: {noul}"));
    }
    let model = v["model"].as_str().ok_or("response missing model")?;
    Ok((noul, model.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bodies() {
        let j = Judge {
            state: "Help!",
            question: "Urgent?",
            criteria: Some(("yes", "no")),
        };
        let q = json!({"gate": {"type": "noul", "instructions": "Urgent?",
            "criteria": {"true": "yes", "false": "no"}}});
        unsafe {
            env::set_var("JEV_API_KEY", "k");
            env::set_var("CLOUDFLARE_API_TOKEN", "t");
            env::set_var("CLOUDFLARE_ACCOUNT_ID", "acct");
        }
        let (url, _, body) = Provider::SystemOne.request(&j, None, false).unwrap();
        assert_eq!(url, "https://api.typesafe.ai/v1/systemone");
        assert_eq!(
            body,
            json!({"model": "jev-latest", "state": "Help!", "questions": q})
        );

        let (url, tok, body) = Provider::Cloudflare.request(&j, None, false).unwrap();
        assert_eq!(
            url,
            "https://api.cloudflare.com/client/v4/accounts/acct/ai/run"
        );
        assert_eq!(tok, "t");
        assert_eq!(
            body,
            json!({"model": "typesafe/jev", "input": {"state": "Help!", "questions": q}})
        );

        let no_criteria = Judge {
            criteria: None,
            ..j
        };
        assert!(questions(&no_criteria)["gate"].get("criteria").is_none());
        assert!(Provider::OpenAI.request(&no_criteria, None, false).is_err());
    }

    #[test]
    fn parsing() {
        let ok =
            json!({"model": "jev-1.13.0", "answers": {"gate": {"type": "noul", "noul": 0.95}}});
        assert_eq!(parse(&ok).unwrap(), (0.95, "jev-1.13.0".into()));
        assert_eq!(parse(&json!({"result": ok})).unwrap().0, 0.95);
        assert!(parse(&json!({"model": "m", "answers": {}})).is_err());
        assert!(parse(&json!({"model": "m", "answers": {"gate": {"noul": "x"}}})).is_err());
        assert!(parse(&json!({"model": "m", "answers": {"gate": {"noul": 1.5}}})).is_err());
        assert!(parse(&json!({"answers": {"gate": {"noul": 0.5}}})).is_err());
    }
}
