//! Auth views are generated from web/auth-ui.tsx at build time. Native code fills
//! escaped data slots, keeping protocol forms script-free and policy server-owned.
#[derive(serde::Deserialize)]
pub struct Pages {
    consent: String,
    logout: String,
    error: String,
    permission: String,
}

impl Pages {
    pub fn load(assets: &str) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(serde_json::from_slice(&std::fs::read(
            std::path::Path::new(assets).join("auth-pages.json"),
        )?)?)
    }
    pub fn consent(
        &self,
        client: &str,
        origin: &str,
        email: &str,
        scope: &str,
        request: &str,
    ) -> String {
        let permissions: String = scope
            .split_whitespace()
            .map(|scope| {
                let (title, description) = match scope {
                    "openid" => ("Sign you in", "Use your Authy account to identify you."),
                    "profile" => ("Read your profile", "Access your name and profile details."),
                    "email" => (
                        "See your email address",
                        "Read the email address on your account.",
                    ),
                    "offline_access" => (
                        "Keep you signed in",
                        "Refresh access without asking you to sign in each time.",
                    ),
                    other => (other, "Access this requested permission."),
                };
                fill(
                    &self.permission,
                    &[
                        ("title", Slot::Text(title)),
                        ("description", Slot::Text(description)),
                    ],
                )
            })
            .collect();
        fill(
            &self.consent,
            &[
                ("client", Slot::Text(client)),
                ("origin", Slot::Text(origin)),
                ("email", Slot::Text(email)),
                ("request", Slot::Text(request)),
                ("permissions", Slot::Markup(&permissions)),
            ],
        )
    }
    pub fn logout(&self, request: &str) -> String {
        fill(&self.logout, &[("request", Slot::Text(request))])
    }
    pub fn error(&self, message: &str) -> String {
        fill(&self.error, &[("message", Slot::Text(message))])
    }
}

pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
enum Slot<'a> {
    Text(&'a str),
    Markup(&'a str),
}
fn fill(template: &str, slots: &[(&str, Slot<'_>)]) -> String {
    let mut output = String::new();
    let mut remaining = template;
    while let Some(start) = remaining.find("{{") {
        output.push_str(&remaining[..start]);
        let Some(end) = remaining[start + 2..].find("}}") else {
            output.push_str(&remaining[start..]);
            return output;
        };
        let key = &remaining[start + 2..start + 2 + end];
        if let Some((_, value)) = slots.iter().find(|(name, _)| *name == key) {
            match value {
                Slot::Text(text) => output.push_str(&escape(text)),
                Slot::Markup(html) => output.push_str(html),
            }
        }
        remaining = &remaining[start + 2 + end + 2..];
    }
    output.push_str(remaining);
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_data_is_escaped_once_and_cannot_introduce_template_slots() {
        assert_eq!(
            fill(
                "<b>{{client}}</b><input value=\"{{request}}\">{{permissions}}",
                &[
                    ("client", Slot::Text("<script>{{permissions}}</script>")),
                    ("request", Slot::Text("\" autofocus onfocus='bad'")),
                    ("permissions", Slot::Markup("<li>Safe</li>")),
                ]
            ),
            "<b>&lt;script&gt;{{permissions}}&lt;/script&gt;</b><input value=\"&quot; autofocus onfocus=&#39;bad&#39;\"><li>Safe</li>"
        );
    }
}
