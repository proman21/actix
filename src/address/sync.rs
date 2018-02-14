use futures::sync::oneshot::{Sender, Receiver};

use actor::Actor;
use handler::{Handler, Message};

use super::envelope::{ToEnvelope, SyncEnvelope, SyncMessageEnvelope};
use super::sync_channel::{SyncSender, SyncAddressSender};
use super::{Request, Recipient, RecipientRequest};
use super::{Destination, MessageDestination, MessageRecipient, SendError};


/// Sync destination of the actor. Actor can run in different thread
pub struct Syn;

impl<A: Actor> Destination<A> for Syn
{
    type Transport = SyncAddressSender<A>;

    /// Indicates if actor is still alive
    fn connected(tx: &Self::Transport) -> bool {
        tx.connected()
    }
}

impl<A: Actor, M> MessageDestination<A, M> for Syn
    where A: Handler<M>, A::Context: ToEnvelope<Self, A, M>,
          M: Message + Send + 'static, M::Result: Send,
{
    type Envelope = SyncEnvelope<A>;
    type ResultSender = Sender<M::Result>;
    type ResultReceiver = Receiver<M::Result>;

    fn do_send(tx: &Self::Transport, msg: M) {
        let _ = tx.do_send(msg);
    }

    fn try_send(tx: &Self::Transport, msg: M) -> Result<(), SendError<M>> {
        tx.try_send(msg, false)
    }

    fn send(tx: &Self::Transport, msg: M) -> Request<Self, A, M> {
        match tx.send(msg) {
            Ok(rx) => Request::new(Some(rx), None),
            Err(SendError::Full(msg)) => Request::new(None, Some((tx.clone(), msg))),
            Err(SendError::Closed(_)) => Request::new(None, None),
        }
    }

    fn recipient(tx: Self::Transport) -> Recipient<Self, M> {
        Recipient::new(tx.into_sender())
    }
}

impl<M> MessageRecipient<M> for Syn
    where M: Message + Send + 'static, M::Result: Send
{
    type Transport = Box<SyncSender<M>>;
    type Envelope = SyncMessageEnvelope<M>;
    type ResultReceiver = Receiver<M::Result>;

    fn do_send(tx: &Self::Transport, msg: M) -> Result<(), SendError<M>> {
        tx.do_send(msg)
    }

    fn try_send(tx: &Self::Transport, msg: M) -> Result<(), SendError<M>> {
        tx.try_send(msg)
    }

    fn send(tx: &Self::Transport, msg: M) -> RecipientRequest<Self, M> {
        match tx.send(msg) {
            Ok(rx) => RecipientRequest::new(Some(rx), None),
            Err(SendError::Full(msg)) =>
                RecipientRequest::new(None, Some((tx.boxed(), msg))),
            Err(SendError::Closed(_)) =>
                RecipientRequest::new(None, None),
        }
    }

    fn clone(tx: &Self::Transport) -> Self::Transport {
        tx.boxed()
    }
}
