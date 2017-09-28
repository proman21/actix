use std;
use futures::{Future, Async, Poll, Stream};

use actor::Actor;
use arbiter::{Arbiter, Execute};
use address::{Address, SyncAddress, Proxy};
use context::{Context, ContextProtocol};
use factory::ActorFactory;
use queue::{sync, unsync};

/// Actor supervisor
///
/// Some message processing garantees considirations. `Supervisor` can not garantee
/// that actor successfully process incoming message. If actor fails during
/// message processing, this message can not be recovered. But sender
/// would receive `Err(Cancelled)` error if actor fails to process message.
///
/// ## Example
///
/// ```rust
/// extern crate actix;
///
/// use actix::prelude::*;
///
/// // message
/// struct Die;
///
/// struct MyActor;
///
/// impl Actor for MyActor {}
///
/// // To use actor with supervisor actor's `ActorFactory` is required
/// impl ActorFactory<MyActor> for MyActor {
///     fn create(&mut self, _: &mut Context<MyActor>) -> MyActor {
///         MyActor
///     }
/// }
///
/// impl MessageResponse<Die> for MyActor {
///     type Item = ();
///     type Error = ();
/// }
///
/// impl MessageHandler<Die> for MyActor {
///
///     fn handle(&mut self, _: Die, ctx: &mut Context<MyActor>) -> MessageFuture<Self, Die> {
///         ctx.stop();
///         Arbiter::system().send(actix::SystemExit(0));
///         ().to_result()
///     }
/// }
///
/// fn main() {
///     let sys = System::new("test".to_owned());
///
///     let (addr, _) = Supervisor::start(MyActor, false);
///
///     addr.send(Die);
///     sys.run();
/// }
/// ```
pub struct Supervisor<A: Actor, F: ActorFactory<A>> {
    factory: F,
    lazy: bool,
    actor: Option<ActorCell<A>>,
    msgs: unsync::UnboundedReceiver<ContextProtocol<A>>,
    sync_msgs: sync::UnboundedReceiver<Proxy<A>>,
}

struct ActorCell<A: Actor> {
    ctx: Context<A>,
    addr: unsync::UnboundedSender<ContextProtocol<A>>,
}

impl<A, F> Supervisor<A, F>
    where A: Actor,
          F: ActorFactory<A> + 'static,
{
    /// Start new supervised actor. Depends on `lazy` argument actor could be started
    /// immidietly or on first incoming message.
    pub fn start(factory: F, lazy: bool) -> (Address<A>, SyncAddress<A>) {
        let rx = unsync::unbounded();
        let (stx, srx) = sync::unbounded();
        let mut supervisor = Supervisor {
            factory: factory,
            lazy: lazy,
            actor: None,
            msgs: rx,
            sync_msgs: srx,
        };
        let addr = Address::new(supervisor.msgs.sender());
        let saddr = SyncAddress::new(stx);

        Arbiter::handle().spawn(supervisor);
        (addr, saddr)
    }

    /// Start new supervised actor in arbiter's thread. Depends on `lazy` argument
    /// actor could be started immidietly or on first incoming message.
    pub fn start_in(addr: SyncAddress<Arbiter>, factory: F, lazy: bool) -> Option<SyncAddress<A>>
        where F: Send
    {
        if addr.is_closed() {
            None
        } else {
            let (tx, rx) = sync::unbounded();

            addr.send(Execute::new(move || -> Result<(), ()> {
                let lrx = unsync::unbounded();
                let supervisor = Supervisor {
                    factory: factory,
                    lazy: lazy,
                    actor: None,
                    msgs: lrx,
                    sync_msgs: rx,
                };
                Arbiter::handle().spawn(supervisor);
                Ok(())
            }));

            if addr.is_closed() {
                None
            } else {
                Some(SyncAddress::new(tx))
            }
        }
    }

    fn get_cell(&mut self) -> &mut ActorCell<A> {
        if self.actor.is_none() {
            self.restart()
        }
        self.actor.as_mut().unwrap()
    }

    fn restart(&mut self) {
        let mut ctx = Context::new(unsafe{std::mem::uninitialized()});

        let addr = ctx.address_cell().unsync_sender();
        let act = self.factory.create(&mut ctx);
        let old = ctx.replace_actor(act);
        std::mem::forget(old);

        self.actor = Some(ActorCell {ctx: ctx, addr: addr});
    }
}

#[doc(hidden)]
impl<A, F> Future for Supervisor<A, F>
    where A: Actor,
          F: ActorFactory<A> + 'static
{
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            if !self.lazy {
                // poll supervised actor
                match self.get_cell().ctx.poll() {
                    Ok(Async::NotReady) => (),
                    Ok(Async::Ready(_)) | Err(_) => {
                        self.restart();
                    }
                }
            }

            let mut not_ready = true;

            // check messages
            match self.msgs.poll() {
                Ok(Async::Ready(Some(msg))) => {
                    not_ready = false;
                    match msg {
                        ContextProtocol::SyncAddress(tx) => {
                            let _ = tx.send(SyncAddress::new(self.sync_msgs.sender()));
                        }
                        // if Actor message queue is dead, restart
                        msg => if self.get_cell().addr.unbounded_send(msg).is_err() {
                            self.restart();
                        },
                    }
                }
                Ok(Async::NotReady) | Ok(Async::Ready(None)) | Err(_) => (),
            }

            // check remote messages. we still use local queue for remote message,
            // because actor runs in same context as supervisor
            match self.sync_msgs.poll() {
                Ok(Async::Ready(Some(msg))) => {
                    not_ready = false;
                    if self.get_cell()
                        .addr.unbounded_send(ContextProtocol::Envelope(msg)).is_err()
                    {
                        // if Actor message queue is dead, restart
                        self.restart();
                    }
                },
                Ok(Async::NotReady) | Ok(Async::Ready(None)) | Err(_) => (),
            }

            // are we done
            if not_ready {
                return Ok(Async::NotReady)
            }

            if self.lazy {
                // poll supervised actor
                match self.get_cell().ctx.poll() {
                    Ok(Async::NotReady) => (),
                    Ok(Async::Ready(_)) | Err(_) => {
                        self.lazy = false;
                        self.restart();
                    }
                }
            }
        }
    }
}