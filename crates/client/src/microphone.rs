// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Outgoing microphone audio. See `design/live-audio.md` and `design/speech.md`.

use protocol::{AudioFrame, ClientMessage, Gate};

use crate::{SendError, Sender};

/// Outgoing frames per second: 20 ms frames.
const FRAMES_PER_SECOND: u32 = 50;

/// Turns captured microphone samples into the participant's one stream of audio frames, each carrying its gate.
///
/// The application captures audio on whatever thread its platform uses and passes the samples to [`Self::push`]; this splits them into 20 ms frames, numbers them and sends them through the [`Sender`]. Nothing is sent while the gate is [`Gate::Off`].
///
/// Push-to-talk is an utterance: [`Self::begin`] sends `UtteranceBegin` and switches the gate to [`Gate::Agent`], so the audio that follows is both heard by the other participants and transcribed for the agent; [`Self::end`] sends the rest of the audio and `UtteranceEnd`, and [`Self::cancel`] discards it. The gate then returns to the one set with [`Self::set_gate`], which is `Off` until changed. A failure to deliver the utterance is reported by the client as [`crate::Event::UtteranceFailed`].
pub struct Microphone {
    sender: Sender,
    sample_rate: u32,
    /// Samples per frame.
    frame: usize,
    /// Samples short of a whole frame, all captured with the current gate.
    pending: Vec<i16>,
    /// The gate outside push-to-talk.
    gate: Gate,
    utterance: Option<String>,
    sequence: u32,
    /// Samples captured so far, including those not sent: the sender's clock for frame timestamps.
    captured: u64,
}

impl Microphone {
    /// Send 16-bit mono audio captured at `sample_rate`, which must be between 8 and 48 kHz.
    ///
    /// # Panics
    ///
    /// If `sample_rate` is outside that range, which the protocol does not carry.
    pub fn new(sender: Sender, sample_rate: u32) -> Self {
        assert!(
            (8_000..=48_000).contains(&sample_rate),
            "unsupported microphone sample rate {sample_rate}"
        );
        Self {
            sender,
            sample_rate,
            frame: (sample_rate / FRAMES_PER_SECOND) as usize,
            pending: Vec::new(),
            gate: Gate::Off,
            utterance: None,
            sequence: 0,
            captured: 0,
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// The gate of the audio being sent now: [`Gate::Agent`] during an utterance, otherwise the one set with [`Self::set_gate`].
    pub fn gate(&self) -> Gate {
        if self.utterance.is_some() {
            Gate::Agent
        } else {
            self.gate
        }
    }

    /// The open utterance, between [`Self::begin`] and its end or cancellation.
    pub fn utterance(&self) -> Option<&str> {
        self.utterance.as_deref()
    }

    /// Set the gate outside push-to-talk: [`Gate::Off`] or [`Gate::Live`]. During an utterance it applies once the utterance ends. `Agent` audio is only sent within an utterance, so [`Gate::Agent`] is taken as `Live`.
    pub fn set_gate(&mut self, gate: Gate) -> Result<(), SendError> {
        let gate = if gate == Gate::Agent {
            Gate::Live
        } else {
            gate
        };
        if gate == self.gate {
            return Ok(());
        }
        if self.utterance.is_some() {
            self.gate = gate;
            return Ok(());
        }
        self.flush()?;
        self.gate = gate;
        self.sender.send(ClientMessage::Gate { gate })
    }

    /// Start push-to-talk as the utterance `utterance`, which must be unique for this participant, such as a random or time-based identifier. An utterance already open is cancelled first.
    pub fn begin(&mut self, utterance: impl Into<String>) -> Result<(), SendError> {
        if self.utterance.is_some() {
            self.cancel()?;
        }
        self.flush()?;
        let utterance = utterance.into();
        self.sender
            .send(ClientMessage::Gate { gate: Gate::Agent })?;
        self.sender.send(ClientMessage::UtteranceBegin {
            utterance: utterance.clone(),
        })?;
        self.utterance = Some(utterance);
        Ok(())
    }

    /// Finish push-to-talk: send the audio captured so far, then `UtteranceEnd`. The server transcribes it and queues the text for the agent. Does nothing without an open utterance.
    pub fn end(&mut self) -> Result<(), SendError> {
        if self.utterance.is_none() {
            return Ok(());
        }
        self.flush()?;
        let utterance = self.utterance.take().expect("checked above");
        self.sender
            .send(ClientMessage::UtteranceEnd { utterance })?;
        self.sender.send(ClientMessage::Gate { gate: self.gate })
    }

    /// Abandon push-to-talk without a transcript. Audio not yet sent is discarded. Does nothing without an open utterance.
    pub fn cancel(&mut self) -> Result<(), SendError> {
        let Some(utterance) = self.utterance.take() else {
            return Ok(());
        };
        self.captured += self.pending.len() as u64;
        self.pending.clear();
        self.sender
            .send(ClientMessage::UtteranceCancel { utterance })?;
        self.sender.send(ClientMessage::Gate { gate: self.gate })
    }

    /// Add captured samples, sending each whole frame with the current gate. While the gate is off they are discarded.
    pub fn push(&mut self, samples: &[i16]) -> Result<(), SendError> {
        if self.gate() == Gate::Off {
            self.captured += samples.len() as u64;
            return Ok(());
        }
        let mut samples = samples;
        while !samples.is_empty() {
            let take = (self.frame - self.pending.len()).min(samples.len());
            self.pending.extend_from_slice(&samples[..take]);
            samples = &samples[take..];
            if self.pending.len() == self.frame {
                self.flush()?;
            }
        }
        Ok(())
    }

    /// Add captured samples in the range -1 to 1, as [`Self::push`] does.
    pub fn push_float(&mut self, samples: &[f32]) -> Result<(), SendError> {
        let samples: Vec<i16> = samples
            .iter()
            .map(|sample| (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16)
            .collect();
        self.push(&samples)
    }

    /// Send the pending samples as a frame, short if need be.
    fn flush(&mut self) -> Result<(), SendError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let samples = std::mem::take(&mut self.pending);
        let start = self.captured;
        self.captured += samples.len() as u64;
        let frame = AudioFrame {
            gate: self.gate(),
            source: 0,
            sequence: self.sequence,
            timestamp: start * 1_000_000 / u64::from(self.sample_rate),
            sample_rate: self.sample_rate,
            samples,
        };
        self.sequence = self.sequence.wrapping_add(1);
        self.sender.send_audio(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outbox::{Batch, Outbox};
    use protocol::{FrameDecoder, FrameKind};
    use std::task::{Context, Poll, Waker};

    #[derive(Debug, PartialEq)]
    enum Item {
        Control(ClientMessage),
        Audio(AudioFrame),
    }

    fn sent(outbox: &Outbox) -> Vec<Item> {
        let mut context = Context::from_waker(Waker::noop());
        let Poll::Ready(Batch::Frames(bytes)) = outbox.poll_batch(&mut context, usize::MAX) else {
            return Vec::new();
        };
        let mut decoder = FrameDecoder::new();
        decoder.push(&bytes);
        let mut items = Vec::new();
        while let Some(frame) = decoder.next_frame().unwrap() {
            items.push(match frame.kind {
                FrameKind::Control => Item::Control(frame.to_client_message().unwrap()),
                FrameKind::Audio => Item::Audio(frame.to_audio().unwrap()),
            });
        }
        items
    }

    fn frames(items: &[Item]) -> Vec<(Gate, u32, usize)> {
        items
            .iter()
            .filter_map(|item| match item {
                Item::Audio(frame) => Some((frame.gate, frame.sequence, frame.samples.len())),
                Item::Control(_) => None,
            })
            .collect()
    }

    fn controls(items: &[Item]) -> Vec<ClientMessage> {
        items
            .iter()
            .filter_map(|item| match item {
                Item::Control(message) => Some(message.clone()),
                Item::Audio(_) => None,
            })
            .collect()
    }

    fn microphone() -> (Microphone, Sender) {
        let sender = Sender::detached(Outbox::new(1000, 1000));
        sender.outbox().connect();
        (Microphone::new(sender.clone(), 16_000), sender)
    }

    #[test]
    fn nothing_is_sent_while_the_gate_is_off() {
        let (mut microphone, sender) = microphone();
        assert_eq!(microphone.gate(), Gate::Off);
        microphone.push(&[1; 16_000]).unwrap();
        assert!(sent(sender.outbox()).is_empty());
    }

    #[test]
    fn live_audio_is_sent_in_numbered_twenty_millisecond_frames() {
        let (mut microphone, sender) = microphone();
        microphone.set_gate(Gate::Live).unwrap();
        // Ten milliseconds stays pending until the frame is whole.
        microphone.push(&[1; 160]).unwrap();
        microphone.push(&[1; 500]).unwrap();
        let items = sent(sender.outbox());
        assert_eq!(controls(&items), [ClientMessage::Gate { gate: Gate::Live }]);
        assert_eq!(frames(&items), [(Gate::Live, 0, 320), (Gate::Live, 1, 320)]);
        let Item::Audio(second) = &items[2] else {
            panic!("{items:?}")
        };
        assert_eq!(second.timestamp, 20_000);
        assert_eq!(second.sample_rate, 16_000);
    }

    #[test]
    fn push_to_talk_sends_an_utterance_of_agent_frames_with_its_tail() {
        let (mut microphone, sender) = microphone();
        microphone.push(&[1; 100]).unwrap();
        microphone.begin("u1").unwrap();
        assert_eq!(microphone.gate(), Gate::Agent);
        assert_eq!(microphone.utterance(), Some("u1"));
        microphone.push(&[2; 700]).unwrap();
        microphone.end().unwrap();
        assert_eq!(microphone.gate(), Gate::Off);
        assert_eq!(microphone.utterance(), None);
        // Released, so nothing more is sent.
        microphone.push(&[3; 320]).unwrap();

        let items = sent(sender.outbox());
        assert_eq!(
            controls(&items),
            [
                ClientMessage::Gate { gate: Gate::Agent },
                ClientMessage::UtteranceBegin {
                    utterance: "u1".into()
                },
                ClientMessage::UtteranceEnd {
                    utterance: "u1".into()
                },
                ClientMessage::Gate { gate: Gate::Off },
            ]
        );
        // Every captured sample of the utterance is sent, the last frame short, before its end.
        assert_eq!(
            frames(&items),
            [
                (Gate::Agent, 0, 320),
                (Gate::Agent, 1, 320),
                (Gate::Agent, 2, 60)
            ]
        );
        let end = items
            .iter()
            .position(|item| matches!(item, Item::Control(ClientMessage::UtteranceEnd { .. })))
            .unwrap();
        assert!(matches!(items[end - 1], Item::Audio(_)));
        let Item::Audio(first) = items
            .iter()
            .find(|item| matches!(item, Item::Audio(_)))
            .unwrap()
        else {
            unreachable!()
        };
        // The audio discarded while off still counts on the sender's clock.
        assert_eq!(first.timestamp, 100 * 1_000_000 / 16_000);
    }

    #[test]
    fn push_to_talk_returns_to_live_audio() {
        let (mut microphone, sender) = microphone();
        microphone.set_gate(Gate::Live).unwrap();
        microphone.push(&[1; 400]).unwrap();
        microphone.begin("u1").unwrap();
        microphone.push(&[2; 320]).unwrap();
        // A gate change during the utterance waits for its end.
        microphone.set_gate(Gate::Off).unwrap();
        microphone.set_gate(Gate::Live).unwrap();
        assert_eq!(microphone.gate(), Gate::Agent);
        microphone.end().unwrap();
        microphone.push(&[3; 320]).unwrap();
        let items = sent(sender.outbox());
        // The live tail before the utterance is sent as live audio, not as part of the utterance.
        assert_eq!(
            frames(&items),
            [
                (Gate::Live, 0, 320),
                (Gate::Live, 1, 80),
                (Gate::Agent, 2, 320),
                (Gate::Live, 3, 320)
            ]
        );
        assert_eq!(
            controls(&items).last(),
            Some(&ClientMessage::Gate { gate: Gate::Live })
        );
    }

    #[test]
    fn cancelling_discards_the_unsent_tail() {
        let (mut microphone, sender) = microphone();
        microphone.begin("u1").unwrap();
        microphone.push(&[2; 400]).unwrap();
        microphone.cancel().unwrap();
        // Ending after a cancellation sends nothing.
        microphone.end().unwrap();
        let items = sent(sender.outbox());
        assert_eq!(frames(&items), [(Gate::Agent, 0, 320)]);
        assert_eq!(
            controls(&items),
            [
                ClientMessage::Gate { gate: Gate::Agent },
                ClientMessage::UtteranceBegin {
                    utterance: "u1".into()
                },
                ClientMessage::UtteranceCancel {
                    utterance: "u1".into()
                },
                ClientMessage::Gate { gate: Gate::Off },
            ]
        );
    }

    #[test]
    fn beginning_again_cancels_the_open_utterance() {
        let (mut microphone, sender) = microphone();
        microphone.begin("u1").unwrap();
        microphone.begin("u2").unwrap();
        assert_eq!(microphone.utterance(), Some("u2"));
        let controls = controls(&sent(sender.outbox()));
        assert!(controls.contains(&ClientMessage::UtteranceCancel {
            utterance: "u1".into()
        }));
        assert_eq!(
            controls.last(),
            Some(&ClientMessage::UtteranceBegin {
                utterance: "u2".into()
            })
        );
    }

    #[test]
    fn float_samples_are_converted_and_clamped() {
        let (mut microphone, sender) = microphone();
        microphone.begin("u1").unwrap();
        microphone.push_float(&[0.5, -2.0, 2.0]).unwrap();
        microphone.end().unwrap();
        let items = sent(sender.outbox());
        let Some(Item::Audio(frame)) = items.iter().find(|item| matches!(item, Item::Audio(_)))
        else {
            panic!("{items:?}")
        };
        assert_eq!(frame.samples, [16_383, -32_767, 32_767]);
    }

    #[test]
    #[should_panic(expected = "unsupported microphone sample rate")]
    fn refuses_a_rate_the_protocol_cannot_carry() {
        let sender = Sender::detached(Outbox::new(1, 1));
        Microphone::new(sender, 96_000);
    }
}
