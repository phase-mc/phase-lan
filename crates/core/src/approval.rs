//! The "let the new device in?" question via events, for the program window.
//!
//! The core calls [`Approver::approve`], and [`SessionEvent::ApprovalRequested`] goes onto the bus
//! with a question number; the interface shows the question and answers with
//! [`ApprovalQueue::answer`]. If no answer will come (time ran out, the device left,
//! the session was stopped), the core closes the question itself and sends
//! [`SessionEvent::ApprovalClosed`]; the interface removes the question window.

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use n0_future::boxed::BoxFuture;
use phase_tunnel::{EndpointId, access::Approver};
use serde::Serialize;
use tokio::sync::oneshot;

use crate::events::{EventBus, SessionEvent};

/// A question that has no answer yet.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PendingApproval {
    pub request_id: u64,
    pub device_id: String,
    /// The in-game nickname the device gave (`None` means it did not introduce itself). The nickname
    /// is chosen by the player themselves: the ID is what must be compared.
    pub nickname: Option<String>,
}

struct Pending {
    device_id: String,
    nickname: Option<String>,
    answer: oneshot::Sender<bool>,
}

#[derive(Default)]
struct Inner {
    next_id: AtomicU64,
    pending: Mutex<BTreeMap<u64, Pending>>,
}

/// Queue of "let in?" questions. Cheap to clone: all copies are one queue.
#[derive(Clone)]
pub struct ApprovalQueue {
    bus: EventBus,
    inner: Arc<Inner>,
}

impl std::fmt::Debug for ApprovalQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalQueue")
            .field("pending", &self.pending().len())
            .finish()
    }
}

impl ApprovalQueue {
    pub fn new(bus: EventBus) -> Self {
        Self {
            bus,
            inner: Arc::default(),
        }
    }

    /// The answer to a question. `false` means there is no such question anymore (time ran out, the device left).
    pub fn answer(&self, request_id: u64, allow: bool) -> bool {
        let pending = self
            .inner
            .pending
            .lock()
            .expect("approvals lock")
            .remove(&request_id);
        match pending {
            // The core may have just stopped waiting, in which case the answer is not needed.
            Some(pending) => pending.answer.send(allow).is_ok(),
            None => false,
        }
    }

    /// Questions without an answer (for example, to show them again after the window reloads).
    pub fn pending(&self) -> Vec<PendingApproval> {
        self.inner
            .pending
            .lock()
            .expect("approvals lock")
            .iter()
            .map(|(id, p)| PendingApproval {
                request_id: *id,
                device_id: p.device_id.clone(),
                nickname: p.nickname.clone(),
            })
            .collect()
    }
}

/// Removes a question from the queue when the core stops waiting for an answer (for any reason).
struct CloseOnDrop {
    queue: ApprovalQueue,
    request_id: u64,
}

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        self.queue
            .inner
            .pending
            .lock()
            .expect("approvals lock")
            .remove(&self.request_id);
        self.queue.bus.send(SessionEvent::ApprovalClosed {
            request_id: self.request_id,
        });
    }
}

impl Approver for ApprovalQueue {
    fn approve(&self, id: EndpointId, nickname: Option<String>) -> BoxFuture<bool> {
        let request_id = self.inner.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = oneshot::channel();
        let device_id = id.to_string();
        self.inner.pending.lock().expect("approvals lock").insert(
            request_id,
            Pending {
                device_id: device_id.clone(),
                nickname: nickname.clone(),
                answer: tx,
            },
        );
        self.bus.send(SessionEvent::ApprovalRequested {
            request_id,
            device_id,
            nickname,
        });
        let guard = CloseOnDrop {
            queue: self.clone(),
            request_id,
        };
        Box::pin(async move {
            let _guard = guard;
            // The queue was dropped without an answer: the device is not let in.
            rx.await.unwrap_or(false)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phase_tunnel::SecretKey;

    #[tokio::test]
    async fn answer_reaches_the_core_and_closes_the_question() {
        let bus = EventBus::new();
        let mut events = bus.subscribe();
        let queue = ApprovalQueue::new(bus);
        let id = SecretKey::generate().public();

        let waiting = tokio::spawn(queue.approve(id, Some("GrayIsko1337".into())));
        let SessionEvent::ApprovalRequested {
            request_id,
            device_id,
            nickname,
        } = events.recv().await.unwrap()
        else {
            panic!("expected a question");
        };
        assert_eq!(device_id, id.to_string());
        assert_eq!(nickname.as_deref(), Some("GrayIsko1337"));
        // A question with a nickname is visible after the window reloads too.
        assert_eq!(
            queue.pending(),
            vec![PendingApproval {
                request_id,
                device_id,
                nickname,
            }]
        );

        assert!(queue.answer(request_id, true));
        assert!(waiting.await.unwrap());
        assert_eq!(
            events.recv().await.unwrap(),
            SessionEvent::ApprovalClosed { request_id }
        );
        assert!(queue.pending().is_empty());
        assert!(
            !queue.answer(request_id, true),
            "a repeated answer is not needed"
        );
    }

    #[tokio::test]
    async fn dropped_question_is_closed_and_cannot_be_answered() {
        let bus = EventBus::new();
        let mut events = bus.subscribe();
        let queue = ApprovalQueue::new(bus);

        let question = queue.approve(SecretKey::generate().public(), None);
        let SessionEvent::ApprovalRequested { request_id, .. } = events.recv().await.unwrap()
        else {
            panic!("expected a question");
        };
        drop(question); // the core stopped waiting (time ran out / the device left)
        assert_eq!(
            events.recv().await.unwrap(),
            SessionEvent::ApprovalClosed { request_id }
        );
        assert!(!queue.answer(request_id, true));
        assert!(queue.pending().is_empty());
    }
}
