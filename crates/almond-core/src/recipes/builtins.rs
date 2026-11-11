//! Built-in recipes, shipped in code (ids `builtin:*`). Their text comes from here on every
//! device and app version; the store only keeps the user's preferences for them (hidden,
//! auto-run) as a `recipe` record with the same id.

use almond_types::recipes::Recipe;

pub const TEACH_BACK: &str = "builtin:teach_back";
pub const FORGOT_TO_ASK: &str = "builtin:forgot_to_ask";
pub const COMMUNICATION: &str = "builtin:communication";
pub const DECISIONS: &str = "builtin:decisions";
pub const FOLLOWUP_EMAIL: &str = "builtin:followup_email";

pub fn is_builtin_id(id: &str) -> bool {
    id.starts_with("builtin:")
}

struct Def {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    icon: &'static str,
    prompt: &'static str,
}

const DEFS: [Def; 5] = [
    Def {
        id: TEACH_BACK,
        name: "Teach it back to me",
        description: "Explains the meeting as if teaching you, with key concepts and a short quiz.",
        icon: "🎓",
        prompt: "Teach me what happened in this meeting as if I were a smart newcomer who missed it.

## The big picture
2-3 sentences: what the meeting was about and why it mattered.

## Key concepts
The 3-6 most important ideas, terms, numbers or decisions. For each: a bold name, then a simple explanation in 1-3 sentences, with an example from the meeting where possible.

## How it fits together
One short paragraph connecting the concepts.

## Quiz
3-5 short numbered questions that check understanding (mix recall and reasoning). Then a separate \"Answers\" list with one short answer per question.",
    },
    Def {
        id: FORGOT_TO_ASK,
        name: "Questions I forgot to ask",
        description: "Gaps, unclear decisions and risks, plus follow-up questions for each person.",
        icon: "❓",
        prompt: "Find what was left unclear or unasked in this meeting.

## Gaps and unclear points
Things mentioned but not explained; decisions without an owner, a deadline or a budget; contradictions between speakers.

## Risks
What could go wrong given what was said, and which assumption each risk rests on.

## Questions to ask next
Grouped by person (a heading per person: the name if the transcript states it, else S1, S2, ...). For each person, 1-3 specific follow-up questions, each followed by a short reason why it matters.

Only raise points grounded in the transcript. Skip a section if there is nothing for it.",
    },
    Def {
        id: COMMUNICATION,
        name: "How did I communicate?",
        description: "Talk time, interruptions, filler words and clarity, with one thing to try next time.",
        icon: "🗣️",
        prompt: "Give me (the speaker \"me\") honest, specific feedback on how I communicated in this meeting.
Take every number (talk time and share, turns, longest turn, pace, interruptions, filler words) from the meeting statistics; never estimate them yourself.

## What the numbers say
2-3 sentences on what the statistics say about me compared with the others (talk-time share, turns, interruptions made and received, filler words), quoting the numbers exactly. Do not draw a table: the app shows the statistics table above your answer.

## Clarity
Were my points clear, structured and to the point? Cite 1-2 examples.

## Listening
Did I let others finish, ask questions and build on their points? Cite examples.

## What went well
2-3 specific things, with citations.

## One thing to try next time
A single concrete, actionable suggestion.

If there are no lines from \"me\", say that my microphone was not captured and give feedback on the conversation as a whole.",
    },
    Def {
        id: DECISIONS,
        name: "Decisions & owners",
        description: "A table of every decision with its owner and due date.",
        icon: "✅",
        prompt: "List every decision made in this meeting as a Markdown table with the columns: Decision | Owner | Due date | Source.
- Owner and due date only when the transcript states them, otherwise \"—\".
- Source: the citation(s) for the decision.
Below the table, add \"Open questions\": topics that were discussed but not decided, one bullet each with citations.
If no decisions were made, say so in one sentence and list only the open questions.",
    },
    Def {
        id: FOLLOWUP_EMAIL,
        name: "Follow-up email draft",
        description: "A ready-to-edit follow-up email to the participants.",
        icon: "✉️",
        prompt: "Draft a follow-up email from me to the other participants.

Start with a line \"**Subject:** ...\" (short and specific). Then the email body: a one-line thank-you, a brief recap (3-5 bullets), the decisions, the action items with owners and dates when stated, and the next steps.
Friendly, professional and concise (under 200 words). Use people's names when the transcript states them, otherwise a neutral greeting. Sign off as \"me\" would, without inventing a name.
Exception to the citation rule: the email itself must contain no [index] citations, because it will be sent as is. After the email, add one line \"Sources:\" followed by the citations for the facts used.",
    },
];

/// The built-in recipes with their default settings (not hidden, no auto-run).
pub fn all() -> Vec<Recipe> {
    DEFS.iter()
        .map(|d| Recipe {
            id: d.id.into(),
            name: d.name.into(),
            description: d.description.into(),
            icon: Some(d.icon.into()),
            prompt: d.prompt.into(),
            auto_run: false,
            builtin: true,
            updated_at: 0,
        })
        .collect()
}

pub fn get(id: &str) -> Option<Recipe> {
    all().into_iter().find(|r| r.id == id)
}
