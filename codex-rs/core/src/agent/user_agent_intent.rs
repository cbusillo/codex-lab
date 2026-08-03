use codex_config::agent_defaults::natural_language_agent_aliases;
use codex_extension_api::ExtensionData;
use codex_protocol::user_input::UserInput;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct UserAgentMentions {
    slugs: Vec<String>,
}

impl UserAgentMentions {
    pub(crate) fn from_user_input(input: &[UserInput]) -> Self {
        let mut mentions = Self::default();
        for text in input.iter().filter_map(|item| match item {
            UserInput::Text { text, .. } => Some(text.as_str()),
            _ => None,
        }) {
            mentions.extend_text(text);
        }
        mentions
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.slugs.is_empty()
    }

    pub(crate) fn slugs(&self) -> &[String] {
        &self.slugs
    }

    fn extend(&mut self, other: Self) {
        for slug in other.slugs {
            if !self.slugs.contains(&slug) {
                self.slugs.push(slug);
            }
        }
    }

    fn extend_text(&mut self, text: &str) {
        let normalized = normalize_agent_text(text);
        let tokens = normalized.split_whitespace().collect::<Vec<_>>();
        let mut aliases = natural_language_agent_aliases()
            .iter()
            .map(|(alias, slug)| {
                (
                    normalize_agent_text(alias)
                        .split_whitespace()
                        .map(str::to_string)
                        .collect::<Vec<_>>(),
                    *slug,
                )
            })
            .collect::<Vec<_>>();
        aliases.sort_by(|(left, _), (right, _)| right.len().cmp(&left.len()));

        let mut index = 0;
        while index < tokens.len() {
            let Some((alias, slug)) = aliases.iter().find(|(alias, _)| {
                index + alias.len() <= tokens.len()
                    && tokens[index..index + alias.len()]
                        .iter()
                        .copied()
                        .eq(alias.iter().map(String::as_str))
            }) else {
                index += 1;
                continue;
            };
            if !self.slugs.iter().any(|existing| existing == slug) {
                self.slugs.push((*slug).to_string());
            }
            index += alias.len();
        }
    }
}

pub(crate) fn record_user_agent_mentions(data: &ExtensionData, input: &[UserInput]) {
    let discovered = UserAgentMentions::from_user_input(input);
    if discovered.is_empty() {
        return;
    }
    let mut combined = data
        .get::<UserAgentMentions>()
        .map(|mentions| mentions.as_ref().clone())
        .unwrap_or_default();
    combined.extend(discovered);
    data.insert(combined);
}

fn normalize_agent_text(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> UserInput {
        UserInput::Text {
            text: value.to_string(),
            text_elements: Vec::new(),
        }
    }

    #[test]
    fn extracts_known_agent_mentions_on_token_boundaries() {
        let mentions = UserAgentMentions::from_user_input(&[
            text("Ask Opus and Gemini to review this."),
            text("Then get github-copilot's view."),
        ]);

        assert_eq!(
            mentions.slugs(),
            ["claude-opus-5", "antigravity", "github-copilot"]
        );
    }

    #[test]
    fn prefers_longest_alias_without_losing_separate_mentions() {
        let mentions = UserAgentMentions::from_user_input(&[text(
            "Ask Claude Opus first, then ask Claude separately.",
        )]);

        assert_eq!(mentions.slugs(), ["claude-opus-5", "claude-sonnet-4.6"]);
    }

    #[test]
    fn ignores_generic_and_partial_words() {
        let mentions = UserAgentMentions::from_user_input(&[text(
            "Google the code for opusculum encoding details.",
        )]);

        assert!(mentions.is_empty());
    }

    #[test]
    fn merges_pending_turn_mentions_without_duplicates() {
        let data = ExtensionData::new("turn");
        record_user_agent_mentions(&data, &[text("Ask Opus")]);
        record_user_agent_mentions(&data, &[text("Also ask opus and AGY")]);

        let mentions = data
            .get::<UserAgentMentions>()
            .expect("mentions should be stored");
        assert_eq!(mentions.slugs(), ["claude-opus-5", "antigravity"]);
    }
}
