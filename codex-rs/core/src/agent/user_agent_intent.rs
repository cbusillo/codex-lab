use codex_config::agent_defaults::natural_language_agent_aliases;
use codex_extension_api::ExtensionData;
use codex_protocol::user_input::UserInput;

const MAX_INTENT_CUE_DISTANCE: usize = 4;
const REQUIRED_CUES: &[&str] = &[
    "ask", "bring", "call", "consult", "delegate", "engage", "get", "have", "invoke", "need",
    "pick", "prefer", "request", "send", "spawn", "use", "want",
];
const REJECTED_CUES: &[&str] = &[
    "avoid",
    "dont",
    "except",
    "exclude",
    "excluding",
    "forbid",
    "never",
    "no",
    "not",
    "omit",
    "skip",
    "without",
];
const LIST_CONNECTORS: &[&str] = &["and", "or"];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct UserAgentIntent {
    required: Vec<String>,
    rejected: Vec<String>,
}

impl UserAgentIntent {
    pub(crate) fn from_user_input(input: &[UserInput]) -> Self {
        let mut intent = Self::default();
        for text in input.iter().filter_map(|item| match item {
            UserInput::Text { text, .. } => Some(text.as_str()),
            _ => None,
        }) {
            intent.extend_text(text);
        }
        intent
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.required.is_empty() && self.rejected.is_empty()
    }

    pub(crate) fn required(&self) -> &[String] {
        &self.required
    }

    #[cfg(test)]
    fn rejected(&self) -> &[String] {
        &self.rejected
    }

    pub(crate) fn requires(&self, slug: &str) -> bool {
        self.required.iter().any(|required| required == slug)
    }

    pub(crate) fn rejects(&self, slug: &str) -> bool {
        self.rejected.iter().any(|rejected| rejected == slug)
    }

    fn extend(&mut self, other: Self) {
        for slug in other.required {
            self.insert(slug, AgentIntentPolarity::Required);
        }
        for slug in other.rejected {
            self.insert(slug, AgentIntentPolarity::Rejected);
        }
    }

    fn extend_text(&mut self, text: &str) {
        let tokens = tokenize_agent_text(text);
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

        let mut previous_mention: Option<AgentMention> = None;
        let mut index = 0;
        while index < tokens.len() {
            let Some((alias, slug)) = aliases.iter().find(|(alias, _)| {
                let end = index + alias.len();
                end <= tokens.len()
                    && (index == 0 || tokens[index - 1].source_word != tokens[index].source_word)
                    && (end == tokens.len()
                        || tokens[end].source_word != tokens[end - 1].source_word)
                    && tokens[index..end]
                        .iter()
                        .map(|token| token.value.as_str())
                        .eq(alias.iter().map(String::as_str))
                    && tokens[index..end]
                        .iter()
                        .all(|token| token.segment == tokens[index].segment && !token.blocked)
            }) else {
                index += 1;
                continue;
            };
            let end = index + alias.len();
            let mut polarity = classify_agent_mention(&tokens, index);
            if polarity == AgentIntentPolarity::Incidental
                && let Some(previous) = previous_mention
                && previous.sentence == tokens[index].sentence
                && tokens[previous.end..index]
                    .iter()
                    .all(|token| LIST_CONNECTORS.contains(&token.value.as_str()))
            {
                polarity = previous.polarity;
            }
            if polarity != AgentIntentPolarity::Incidental {
                self.insert((*slug).to_string(), polarity);
                previous_mention = Some(AgentMention {
                    end,
                    sentence: tokens[index].sentence,
                    polarity,
                });
            }
            index = end;
        }
    }

    fn insert(&mut self, slug: String, polarity: AgentIntentPolarity) {
        match polarity {
            AgentIntentPolarity::Required => {
                self.rejected.retain(|rejected| rejected != &slug);
                if !self.requires(&slug) {
                    self.required.push(slug);
                }
            }
            AgentIntentPolarity::Rejected => {
                self.required.retain(|required| required != &slug);
                if !self.rejects(&slug) {
                    self.rejected.push(slug);
                }
            }
            AgentIntentPolarity::Incidental => {}
        }
    }
}

pub(crate) fn record_user_agent_intent(data: &ExtensionData, input: &[UserInput]) {
    let discovered = UserAgentIntent::from_user_input(input);
    if discovered.is_empty() {
        return;
    }
    let mut combined = data
        .get::<UserAgentIntent>()
        .map(|intent| intent.as_ref().clone())
        .unwrap_or_default();
    combined.extend(discovered);
    data.insert(combined);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentIntentPolarity {
    Required,
    Rejected,
    Incidental,
}

#[derive(Debug, Clone, Copy)]
struct AgentMention {
    end: usize,
    sentence: usize,
    polarity: AgentIntentPolarity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentTextToken {
    value: String,
    source_word: usize,
    segment: usize,
    sentence: usize,
    blocked: bool,
}

fn classify_agent_mention(tokens: &[AgentTextToken], start: usize) -> AgentIntentPolarity {
    let segment = tokens[start].segment;
    let lookback_start = start.saturating_sub(MAX_INTENT_CUE_DISTANCE);
    let lookback = &tokens[lookback_start..start];
    let required_position = lookback
        .iter()
        .rposition(|token| REQUIRED_CUES.contains(&token.value.as_str()));
    let rejected_position = lookback
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, token)| rejected_cue_at(lookback, index, token).then_some(index));
    let required_position =
        required_position.filter(|position| lookback[*position].segment == segment);
    let rejected_position =
        rejected_position.filter(|position| lookback[*position].segment == segment);

    match (required_position, rejected_position) {
        (Some(required), Some(rejected))
            if rejected > required || required.saturating_sub(rejected) <= 2 =>
        {
            AgentIntentPolarity::Rejected
        }
        (Some(_), _) => AgentIntentPolarity::Required,
        (None, Some(_)) => AgentIntentPolarity::Rejected,
        (None, None) => AgentIntentPolarity::Incidental,
    }
}

fn rejected_cue_at(tokens: &[AgentTextToken], index: usize, token: &AgentTextToken) -> bool {
    if REJECTED_CUES.contains(&token.value.as_str()) {
        return true;
    }
    let previous = index.checked_sub(1).and_then(|index| tokens.get(index));
    match token.value.as_str() {
        "but" => previous.is_some_and(|previous| previous.value == "anything"),
        "than" => {
            previous.is_some_and(|previous| matches!(previous.value.as_str(), "other" | "rather"))
        }
        "of" => previous.is_some_and(|previous| previous.value == "instead"),
        _ => false,
    }
}

fn tokenize_agent_text(text: &str) -> Vec<AgentTextToken> {
    let characters = text.chars().collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let mut source_word = String::new();
    let mut segment = 0;
    let mut sentence = 0;
    let mut in_code = false;
    let mut index = 0;
    while index < characters.len() {
        let character = characters[index];
        if character == '`' {
            push_source_word(&mut tokens, &mut source_word, segment, sentence);
            while index + 1 < characters.len() && characters[index + 1] == '`' {
                index += 1;
            }
            in_code = !in_code;
            index += 1;
            continue;
        }
        if in_code {
            index += 1;
            continue;
        }
        if character.is_whitespace() {
            push_source_word(&mut tokens, &mut source_word, segment, sentence);
            if character == '\n' {
                segment += 1;
                sentence += 1;
            }
            index += 1;
            continue;
        }
        if character == ',' {
            push_source_word(&mut tokens, &mut source_word, segment, sentence);
            segment += 1;
            index += 1;
            continue;
        }
        if matches!(character, ';' | '!' | '?' | '—' | '–') {
            push_source_word(&mut tokens, &mut source_word, segment, sentence);
            segment += 1;
            sentence += 1;
            index += 1;
            continue;
        }
        if character == '.' {
            let previous_is_alphanumeric = source_word
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric);
            let next_is_alphanumeric = characters
                .get(index + 1)
                .is_some_and(|next| next.is_alphanumeric());
            if previous_is_alphanumeric && next_is_alphanumeric {
                source_word.push(character);
            } else {
                push_source_word(&mut tokens, &mut source_word, segment, sentence);
                segment += 1;
                sentence += 1;
            }
            index += 1;
            continue;
        }
        source_word.push(character);
        index += 1;
    }
    push_source_word(&mut tokens, &mut source_word, segment, sentence);
    tokens
}

fn push_source_word(
    tokens: &mut Vec<AgentTextToken>,
    source_word: &mut String,
    segment: usize,
    sentence: usize,
) {
    if source_word.is_empty() {
        return;
    }
    let blocked = source_word_blocks_agent_alias(source_word);
    let source_without_possessive = source_word
        .strip_suffix("'s")
        .or_else(|| source_word.strip_suffix("’s"))
        .unwrap_or(source_word);
    let compact = source_without_possessive
        .chars()
        .filter(|character| !matches!(character, '\'' | '’'))
        .collect::<String>();
    let source_word_id = tokens
        .last()
        .map(|token| token.source_word + 1)
        .unwrap_or_default();
    for value in normalize_agent_text(&compact).split_whitespace() {
        tokens.push(AgentTextToken {
            value: value.to_string(),
            source_word: source_word_id,
            segment,
            sentence,
            blocked,
        });
    }
    source_word.clear();
}

fn source_word_blocks_agent_alias(source_word: &str) -> bool {
    if source_word
        .chars()
        .any(|character| matches!(character, '/' | '\\' | '@' | '_'))
    {
        return true;
    }
    if has_internal_punctuation(source_word, ':') {
        return true;
    }
    source_word.contains('.')
        && !source_word
            .chars()
            .all(|character| character.is_ascii_digit() || character == '.')
}

fn has_internal_punctuation(source_word: &str, punctuation: char) -> bool {
    let characters = source_word.chars().collect::<Vec<_>>();
    characters.iter().enumerate().any(|(index, character)| {
        *character == punctuation
            && index > 0
            && index + 1 < characters.len()
            && characters[index - 1].is_alphanumeric()
            && characters[index + 1].is_alphanumeric()
    })
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
    fn extracts_explicit_required_agent_intent() {
        let intent = UserAgentIntent::from_user_input(&[
            text("Ask Opus and Gemini to review this."),
            text("Then get github-copilot's view."),
        ]);

        assert_eq!(
            intent.required(),
            ["claude-opus-5", "antigravity", "github-copilot"]
        );
        assert!(intent.rejected().is_empty());
    }

    #[test]
    fn prefers_longest_alias_without_losing_separate_requests() {
        let intent = UserAgentIntent::from_user_input(&[text(
            "Ask Claude Opus first, then ask Claude separately.",
        )]);

        assert_eq!(intent.required(), ["claude-opus-5", "claude-sonnet-4.6"]);
    }

    #[test]
    fn distinguishes_required_rejected_and_incidental_mentions() {
        let intent = UserAgentIntent::from_user_input(&[text(
            "Not Opus—use Sonnet. Compare Gemini APIs, update CLAUDE.md, and preserve Opus's finding. `Ask Fable` is only an example.",
        )]);

        assert_eq!(intent.required(), ["claude-sonnet-4.6"]);
        assert_eq!(intent.rejected(), ["claude-opus-5"]);
    }

    #[test]
    fn rejects_negative_agent_directives() {
        let intent = UserAgentIntent::from_user_input(&[
            text("Do not use Opus."),
            text("Anything except Gemini is fine."),
        ]);

        assert!(intent.required().is_empty());
        assert_eq!(intent.rejected(), ["claude-opus-5", "antigravity"]);
    }

    #[test]
    fn ignores_unscoped_agent_discussion() {
        let intent = UserAgentIntent::from_user_input(&[text(
            "Claude is documented in CLAUDE.md, and Opus's review compared Gemini APIs with Sonnet architecture.",
        )]);

        assert!(intent.is_empty());
    }

    #[test]
    fn scopes_live_review_follow_up_to_the_requested_agent() {
        let intent = UserAgentIntent::from_user_input(&[text(
            "Ask Antigravity to assess whether Opus's mention-gate finding is a release blocker.",
        )]);

        assert_eq!(intent.required(), ["antigravity"]);
        assert!(intent.rejected().is_empty());
    }

    #[test]
    fn ignores_alias_prefixes_inside_longer_identifiers() {
        let intent =
            UserAgentIntent::from_user_input(&[text("Ask claude-opus-5-extended to review this.")]);

        assert!(intent.is_empty());
    }

    #[test]
    fn ignores_aliases_embedded_in_blocked_source_words() {
        let intent = UserAgentIntent::from_user_input(&[text(
            "Ask @opus, path/opus, path\\opus, under_opus, and namespace:opus.",
        )]);

        assert!(intent.is_empty());
    }

    #[test]
    fn merges_pending_turn_intent_with_latest_instruction_winning() {
        let data = ExtensionData::new("turn");
        record_user_agent_intent(&data, &[text("Ask Opus")]);
        record_user_agent_intent(&data, &[text("Actually do not use Opus; use AGY")]);

        let intent = data
            .get::<UserAgentIntent>()
            .expect("intent should be stored");
        assert_eq!(intent.required(), ["antigravity"]);
        assert_eq!(intent.rejected(), ["claude-opus-5"]);

        record_user_agent_intent(&data, &[text("Correction: use Opus after all")]);
        let corrected = data
            .get::<UserAgentIntent>()
            .expect("corrected intent should be stored");
        assert_eq!(corrected.required(), ["antigravity", "claude-opus-5"]);
        assert!(corrected.rejected().is_empty());
    }
}
