//! The conversation loop, as a machine with no I/O in it.
//!
//! # Why the transitions live apart from the audio
//!
//! Everything this coordinates is slow, fallible and hard to reproduce: a
//! microphone, a recogniser that takes a second, a model that takes four, a
//! voice that plays for six. A loop written directly against those can only be
//! tested by talking to it.
//!
//! So the decisions are here — a pure function from state and event to the next
//! state plus an action — and the driving is somewhere that can hold a thread.
//! Every rule below is a test, and none of them needs a microphone.
//!
//! # The shape of a turn
//!
//! ```text
//! Idle ──open──▶ Listening ──speech starts──▶ Hearing
//!                    ▲                            │ silence
//!                    │                            ▼
//!                 FollowUp ◀──done── Speaking ◀─ Working
//!                    │ timeout
//!                    ▼
//!                  Idle
//! ```
//!
//! `FollowUp` is what makes it a conversation rather than a series of
//! commands: after answering, the microphone stays open for a few seconds so
//! the next thing said continues the exchange without reaching for the hotkey.

use std::time::Duration;

use crate::intent::Intent;

/// How long the microphone stays open after an answer, waiting for a follow-up.
///
/// Long enough to draw breath and ask the obvious next question; short enough
/// that walking away from the desk closes it. The microphone light being on is
/// the cost, and it is visible, which is the right way round.
pub const DEFAULT_FOLLOW_UP: Duration = Duration::from_secs(6);

/// Where a turn is. Rendered by the UI, so every variant is something a person
/// can be shown honestly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Microphone closed. Nothing running.
    Idle,
    /// Microphone open, waiting for someone to start.
    Listening,
    /// Someone is talking. Audio is accumulating.
    Hearing,
    /// Transcribing, routing, searching, composing — the part with no sound.
    Working,
    /// The answer is being read aloud.
    Speaking,
    /// Answered; still listening for a follow-up before closing.
    FollowUp,
}

impl Phase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Listening => "listening",
            Self::Hearing => "hearing",
            Self::Working => "working",
            Self::Speaking => "speaking",
            Self::FollowUp => "follow_up",
        }
    }

    /// Whether the microphone is open in this phase.
    ///
    /// Used to decide whether to show that the app is listening. It is
    /// deliberately true during `Speaking`: barge-in only works because the
    /// microphone never closed, and a UI that claimed otherwise would be lying
    /// about a running microphone.
    pub const fn microphone_open(self) -> bool {
        !matches!(self, Self::Idle)
    }
}

/// What happened.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The user asked for a conversation — hotkey, button, or wake word.
    Opened,
    /// Speech started, per the endpointer.
    SpeechStarted,
    /// Speech ended, per the endpointer.
    SpeechEnded,
    /// The utterance was transcribed and routed.
    Understood { text: String, intent: Intent },
    /// Nothing intelligible came back. Whisper returns this often for a cough.
    NotUnderstood,
    /// The memory was stored, or the question answered. `spoken` is what the
    /// voice should say.
    Handled { spoken: Option<String> },
    /// Something failed. The turn does not get stuck; it says so and reopens.
    Failed { message: String },
    /// The voice finished, or was interrupted.
    FinishedSpeaking,
    /// The follow-up window expired with nothing said.
    FollowUpExpired,
    /// The user asked to stop.
    Closed,
}

/// What the driver should do next.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Open the microphone and start delivering frames.
    StartListening,
    /// Close the microphone.
    StopListening,
    /// Begin accumulating this utterance's audio.
    BeginUtterance,
    /// Transcribe and route what was accumulated.
    ProcessUtterance,
    /// Store it.
    Capture { text: String },
    /// Answer it.
    Ask { text: String },
    /// Say this.
    Speak { text: String },
    /// Silence the voice immediately.
    StopSpeaking,
    /// Start the follow-up countdown.
    ArmFollowUp { after: Duration },
}

/// The conversation, as a state machine.
#[derive(Debug, Clone)]
pub struct Turn {
    phase: Phase,
    follow_up: Duration,
}

impl Default for Turn {
    fn default() -> Self {
        Self::new(DEFAULT_FOLLOW_UP)
    }
}

impl Turn {
    pub fn new(follow_up: Duration) -> Self {
        Self {
            phase: Phase::Idle,
            follow_up,
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Advance. Returns what the driver should do, in order.
    pub fn handle(&mut self, event: Event) -> Vec<Action> {
        use Event as E;
        use Phase as P;

        match (self.phase, event) {
            // Opening is idempotent: the hotkey repeats, and a second press
            // while already listening should not restart anything.
            (P::Idle, E::Opened) => {
                self.phase = P::Listening;
                vec![Action::StartListening]
            }
            (_, E::Opened) => Vec::new(),

            // Closing works from anywhere, including mid-sentence. That is the
            // point of a stop button.
            (P::Idle, E::Closed) => Vec::new(),
            (_, E::Closed) => {
                self.phase = P::Idle;
                vec![Action::StopSpeaking, Action::StopListening]
            }

            // Barge-in. Talking over the answer silences it and starts a new
            // utterance — the single behaviour that separates a conversation
            // from a phone menu.
            (P::Speaking, E::SpeechStarted) => {
                self.phase = P::Hearing;
                vec![Action::StopSpeaking, Action::BeginUtterance]
            }
            (P::Listening | P::FollowUp, E::SpeechStarted) => {
                self.phase = P::Hearing;
                vec![Action::BeginUtterance]
            }
            // Already hearing, or busy working. A boundary crossed while the
            // model is running belongs to audio the driver is not recording.
            (_, E::SpeechStarted) => Vec::new(),

            (P::Hearing, E::SpeechEnded) => {
                self.phase = P::Working;
                vec![Action::ProcessUtterance]
            }
            (_, E::SpeechEnded) => Vec::new(),

            (P::Working, E::Understood { text, intent }) => match intent {
                Intent::Record => vec![Action::Capture { text }],
                Intent::Ask => vec![Action::Ask { text }],
            },
            (_, E::Understood { .. }) => Vec::new(),

            // A cough, or the microphone catching a door. Say nothing and go
            // back to listening: announcing every non-utterance would make the
            // app chatter at an empty room.
            (P::Working, E::NotUnderstood) => {
                self.phase = P::Listening;
                Vec::new()
            }
            (_, E::NotUnderstood) => Vec::new(),

            (P::Working, E::Handled { spoken }) => match spoken {
                Some(text) => {
                    self.phase = P::Speaking;
                    vec![Action::Speak { text }]
                }
                None => {
                    self.phase = P::FollowUp;
                    vec![Action::ArmFollowUp {
                        after: self.follow_up,
                    }]
                }
            },
            (_, E::Handled { .. }) => Vec::new(),

            // A failure must not strand the turn in `Working` with the
            // microphone open and nothing listening to it.
            (P::Working, E::Failed { message }) => {
                self.phase = P::Speaking;
                vec![Action::Speak { text: message }]
            }
            (_, E::Failed { .. }) => Vec::new(),

            (P::Speaking, E::FinishedSpeaking) => {
                self.phase = P::FollowUp;
                vec![Action::ArmFollowUp {
                    after: self.follow_up,
                }]
            }
            (_, E::FinishedSpeaking) => Vec::new(),

            (P::FollowUp, E::FollowUpExpired) => {
                self.phase = P::Idle;
                vec![Action::StopListening]
            }
            // Expiry racing a new utterance: the user started talking just as
            // the timer fired. Speech wins — the alternative closes the
            // microphone on someone mid-sentence.
            (_, E::FollowUpExpired) => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened() -> Turn {
        let mut turn = Turn::default();
        assert_eq!(turn.handle(Event::Opened), vec![Action::StartListening]);
        turn
    }

    /// Drive a whole successful question, returning every action in order.
    fn ask_through(turn: &mut Turn, question: &str) -> Vec<Action> {
        let mut actions = Vec::new();
        actions.extend(turn.handle(Event::SpeechStarted));
        actions.extend(turn.handle(Event::SpeechEnded));
        actions.extend(turn.handle(Event::Understood {
            text: question.into(),
            intent: Intent::Ask,
        }));
        actions.extend(turn.handle(Event::Handled {
            spoken: Some("Foi por causa do custo por byte.".into()),
        }));
        actions
    }

    #[test]
    fn a_question_runs_from_silence_to_an_answer_and_waits_for_more() {
        let mut turn = opened();
        assert_eq!(turn.phase(), Phase::Listening);

        let actions = ask_through(&mut turn, "por que não OpenSearch?");
        assert_eq!(
            actions,
            vec![
                Action::BeginUtterance,
                Action::ProcessUtterance,
                Action::Ask {
                    text: "por que não OpenSearch?".into()
                },
                Action::Speak {
                    text: "Foi por causa do custo por byte.".into()
                },
            ]
        );
        assert_eq!(turn.phase(), Phase::Speaking);

        assert_eq!(
            turn.handle(Event::FinishedSpeaking),
            vec![Action::ArmFollowUp {
                after: DEFAULT_FOLLOW_UP
            }]
        );
        assert_eq!(turn.phase(), Phase::FollowUp);
    }

    #[test]
    fn a_statement_is_captured_rather_than_asked() {
        let mut turn = opened();
        turn.handle(Event::SpeechStarted);
        turn.handle(Event::SpeechEnded);

        assert_eq!(
            turn.handle(Event::Understood {
                text: "o deploy saiu às três".into(),
                intent: Intent::Record,
            }),
            vec![Action::Capture {
                text: "o deploy saiu às três".into()
            }]
        );
    }

    #[test]
    fn talking_over_the_answer_silences_it_and_starts_a_new_utterance() {
        // The whole reason `Speaker::stop` is on the port. Without this the app
        // finishes its sentence while the user is already talking, and the
        // microphone records both.
        let mut turn = opened();
        ask_through(&mut turn, "por que não OpenSearch?");
        assert_eq!(turn.phase(), Phase::Speaking);

        assert_eq!(
            turn.handle(Event::SpeechStarted),
            vec![Action::StopSpeaking, Action::BeginUtterance],
            "barge-in must silence before it records"
        );
        assert_eq!(turn.phase(), Phase::Hearing);
    }

    #[test]
    fn a_follow_up_continues_without_the_hotkey() {
        let mut turn = opened();
        ask_through(&mut turn, "primeira");
        turn.handle(Event::FinishedSpeaking);
        assert_eq!(turn.phase(), Phase::FollowUp);

        // Speaking again during the window is a new utterance, not a new turn.
        assert_eq!(
            turn.handle(Event::SpeechStarted),
            vec![Action::BeginUtterance]
        );
        assert_eq!(turn.phase(), Phase::Hearing);
    }

    #[test]
    fn the_window_closing_is_what_ends_the_conversation() {
        let mut turn = opened();
        ask_through(&mut turn, "pergunta");
        turn.handle(Event::FinishedSpeaking);

        assert_eq!(
            turn.handle(Event::FollowUpExpired),
            vec![Action::StopListening]
        );
        assert_eq!(turn.phase(), Phase::Idle);
        assert!(!turn.phase().microphone_open());
    }

    #[test]
    fn an_expiry_that_races_a_new_utterance_loses() {
        // The timer fires as the user starts the next sentence. Closing the
        // microphone here cuts them off mid-word.
        let mut turn = opened();
        ask_through(&mut turn, "pergunta");
        turn.handle(Event::FinishedSpeaking);
        turn.handle(Event::SpeechStarted);
        assert_eq!(turn.phase(), Phase::Hearing);

        assert!(
            turn.handle(Event::FollowUpExpired).is_empty(),
            "a late timer must not close the microphone on someone talking"
        );
        assert_eq!(turn.phase(), Phase::Hearing);
    }

    #[test]
    fn a_cough_goes_back_to_listening_without_saying_anything() {
        let mut turn = opened();
        turn.handle(Event::SpeechStarted);
        turn.handle(Event::SpeechEnded);
        assert_eq!(turn.phase(), Phase::Working);

        assert!(
            turn.handle(Event::NotUnderstood).is_empty(),
            "the app must not announce every noise it failed to parse"
        );
        assert_eq!(turn.phase(), Phase::Listening);
    }

    #[test]
    fn a_failure_is_spoken_rather_than_leaving_the_turn_stuck() {
        let mut turn = opened();
        turn.handle(Event::SpeechStarted);
        turn.handle(Event::SpeechEnded);

        assert_eq!(
            turn.handle(Event::Failed {
                message: "não consegui responder agora".into()
            }),
            vec![Action::Speak {
                text: "não consegui responder agora".into()
            }]
        );
        // And from there it reaches `FollowUp` like any other answer, rather
        // than sitting in `Working` with an open microphone forever.
        turn.handle(Event::FinishedSpeaking);
        assert_eq!(turn.phase(), Phase::FollowUp);
    }

    #[test]
    fn a_capture_that_says_nothing_still_reaches_the_follow_up_window() {
        let mut turn = opened();
        turn.handle(Event::SpeechStarted);
        turn.handle(Event::SpeechEnded);
        turn.handle(Event::Understood {
            text: "algo".into(),
            intent: Intent::Record,
        });

        assert_eq!(
            turn.handle(Event::Handled { spoken: None }),
            vec![Action::ArmFollowUp {
                after: DEFAULT_FOLLOW_UP
            }]
        );
        assert_eq!(turn.phase(), Phase::FollowUp);
    }

    #[test]
    fn closing_works_from_any_phase_including_mid_sentence() {
        for reach in [
            Phase::Listening,
            Phase::Hearing,
            Phase::Working,
            Phase::Speaking,
        ] {
            let mut turn = opened();
            match reach {
                Phase::Listening => {}
                Phase::Hearing => {
                    turn.handle(Event::SpeechStarted);
                }
                Phase::Working => {
                    turn.handle(Event::SpeechStarted);
                    turn.handle(Event::SpeechEnded);
                }
                Phase::Speaking => {
                    ask_through(&mut turn, "q");
                }
                _ => unreachable!(),
            }
            assert_eq!(turn.phase(), reach);

            assert_eq!(
                turn.handle(Event::Closed),
                vec![Action::StopSpeaking, Action::StopListening],
                "stopping from {reach:?} must silence and close"
            );
            assert_eq!(turn.phase(), Phase::Idle);
        }
    }

    #[test]
    fn opening_twice_does_not_restart_anything() {
        let mut turn = opened();
        assert!(
            turn.handle(Event::Opened).is_empty(),
            "the hotkey repeats; a second press must be a no-op"
        );
        assert_eq!(turn.phase(), Phase::Listening);
    }

    #[test]
    fn the_microphone_is_honestly_reported_as_open_while_speaking() {
        // Barge-in only works because the microphone never closed. A UI that
        // showed it closed here would be describing a microphone that is on.
        let mut turn = opened();
        ask_through(&mut turn, "q");
        assert_eq!(turn.phase(), Phase::Speaking);
        assert!(turn.phase().microphone_open());
    }

    #[test]
    fn the_wire_names_are_stable_for_the_frontend() {
        assert_eq!(Phase::Idle.as_str(), "idle");
        assert_eq!(Phase::FollowUp.as_str(), "follow_up");
        assert!(!Phase::Idle.microphone_open());
    }
}
