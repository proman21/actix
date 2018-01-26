use std::{mem, fmt};

use actor::{Actor, AsyncContext};

pub use addr::Address;
pub use local::LocalAddress;
pub use context::AsyncContextAddress;

pub enum SendError<T> {
    NotReady(T),
    Closed(T),
}

impl<T> fmt::Debug for SendError<T> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            SendError::NotReady(_) => write!(fmt, "SendError::NotReady(..)"),
            SendError::Closed(_) => write!(fmt, "SendError::Closed(..)"),
        }
    }
}

impl<T> fmt::Display for SendError<T> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            SendError::NotReady(_) => write!(fmt, "send failed because receiver is full"),
            SendError::Closed(_) => write!(fmt, "send failed because receiver is gone"),
        }
    }
}

/// Trait give access to actor's address
pub trait ActorAddress<A, T> where A: Actor {
    /// Returns actor's address for specific context
    fn get(ctx: &mut A::Context) -> T;
}

impl<A> ActorAddress<A, LocalAddress<A>> for A
    where A: Actor, A::Context: AsyncContext<A> + AsyncContextAddress<A>
{
    fn get(ctx: &mut A::Context) -> LocalAddress<A> {
        ctx.local()
    }
}

impl<A> ActorAddress<A, Address<A>> for A
    where A: Actor, A::Context: AsyncContext<A> + AsyncContextAddress<A>
{
    fn get(ctx: &mut A::Context) -> Address<A> {
        ctx.remote()
    }
}

impl<A> ActorAddress<A, (LocalAddress<A>, Address<A>)> for A
    where A: Actor, A::Context: AsyncContext<A> + AsyncContextAddress<A>
{
    fn get(ctx: &mut A::Context) -> (LocalAddress<A>, Address<A>) {
        (ctx.local(), ctx.remote())
    }
}

impl<A> ActorAddress<A, ()> for A where A: Actor {
    fn get(_: &mut A::Context) -> () {
        ()
    }
}

/// Subscriber trait describes ability of actor to receive one specific message
///
/// You can get subscriber with `Address::subscriber()` or
/// `Address::subscriber()` methods. Both methods returns boxed trait object.
///
/// It is possible to use `Clone::clone()` method to get cloned subscriber.
pub trait Subscriber<M: 'static> {
    /// Send buffered message
    fn send(&self, msg: M) -> Result<(), SendError<M>>;

    #[doc(hidden)]
    /// Create boxed clone of the current subscriber
    fn boxed(&self) -> Box<Subscriber<M>>;
}

/// Convenience impl to allow boxed Subscriber objects to be cloned using `Clone.clone()`.
impl<M: 'static> Clone for Box<Subscriber<M>> {
    fn clone(&self) -> Box<Subscriber<M>> {
        self.boxed()
    }
}

/// Convenience impl to allow boxed Subscriber objects to be cloned using `Clone.clone()`.
impl<M: 'static> Clone for Box<Subscriber<M> + Send> {
    fn clone(&self) -> Box<Subscriber<M> + Send> {
        // simplify ergonomics of `+Send` subscriber, otherwise
        // it would require new trait with custom `.boxed()` method.
        unsafe { mem::transmute(self.boxed()) }
    }
}
