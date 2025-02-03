use std::{
    any::Any,
    fmt::Debug,
    future::IntoFuture,
    hash::{Hash, Hasher},
    marker::PhantomData,
    ops::Deref,
};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{
    debug::{ValueDebug, ValueDebugFormat, ValueDebugFormatString},
    trace::{TraceRawVcs, TraceRawVcsContext},
    vc::Vc,
    ResolveTypeError, Upcast, VcRead, VcTransparentRead, VcValueTrait, VcValueType,
};

/// A "subtype" (via [`Deref`]) of [`Vc`] that represents a specific [`Vc::cell`]/`.cell()` or
/// [`ResolvedVc::cell`]/`.resolved_cell()` constructor call within [a task][macro@crate::function].
///
/// Unlike [`Vc`], `ResolvedVc`:
///
/// - Does not potentially refer to task-local information, meaning that it implements
///   [`NonLocalValue`], and can be used in any [`#[turbo_tasks::value]`][macro@crate::value].
///
/// - Has only one potential internal representation, meaning that it has a saner equality
///   definition.
///
/// - Points to a concrete value with a type, and is therefore [cheap to
///   downcast][ResolvedVc::try_downcast].
///
///
/// ## Construction
///
/// There are a few ways to construct a `ResolvedVc`, in order of preference:
///
/// 1. Given a [value][VcValueType], construct a `ResolvedVc` using [`ResolvedVc::cell`] (for
///    "transparent" values) or by calling the generated `.resolved_cell()` constructor on the value
///    type.
///
/// 2. Given an argument to a function using the [`#[turbo_tasks::function]`][macro@crate::function]
///    macro, change the argument's type to a `ResolvedVc`. The [rewritten external signature] will
///    still use [`Vc`], but when the function is called, the [`Vc`] will be resolved.
///
/// 3. Given a [`Vc`], use [`.to_resolved().await?`][Vc::to_resolved].
///
///
/// ## Equality & Hashing
///
/// Equality between two `ResolvedVc`s means that both have an identical in-memory representation
/// and point to the same cell. The implementation of [`Hash`] has similar behavior.
///
/// If `.await`ed at the same time, both would likely resolve to the same [`ReadRef`], though it is
/// possible that they may not if the cell is invalidated between `.await`s.
///
/// Because equality is a synchronous operation that cannot read the cell contents, even if the
/// `ResolvedVc`s are not equal, it is possible that if `.await`ed, both `ResolvedVc`s could point
/// to the same or equal values.
///
///
/// [`NonLocalValue`]: crate::NonLocalValue
/// [rewritten external signature]: https://turbopack-rust-docs.vercel.sh/turbo-engine/tasks.html#external-signature-rewriting
/// [`ReadRef`]: crate::ReadRef
#[derive(Serialize, Deserialize)]
#[serde(transparent, bound = "")]
pub struct ResolvedVc<T>
where
    T: ?Sized,
{
    // no-resolved-vc(kdy1): This is a resolved Vc, so we don't need to resolve it again
    pub(crate) node: Vc<T>,
}

impl<T> ResolvedVc<T>
where
    T: ?Sized,
{
    /// This function exists to intercept calls to Vc::to_resolved through dereferencing
    /// a ResolvedVc. Converting to Vc and re-resolving it puts unnecessary stress on
    /// the turbo tasks engine.
    #[deprecated(note = "No point in resolving a vc that is already resolved")]
    pub async fn to_resolved(self) -> Result<Self> {
        Ok(self)
    }
    #[deprecated(note = "No point in resolving a vc that is already resolved")]
    pub async fn resolve(self) -> Result<Vc<T>> {
        Ok(self.node)
    }
}

impl<T> Copy for ResolvedVc<T> where T: ?Sized {}

impl<T> Clone for ResolvedVc<T>
where
    T: ?Sized,
{
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Deref for ResolvedVc<T>
where
    T: ?Sized,
{
    type Target = Vc<T>;

    fn deref(&self) -> &Self::Target {
        &self.node
    }
}

impl<T> PartialEq<ResolvedVc<T>> for ResolvedVc<T>
where
    T: ?Sized,
{
    fn eq(&self, other: &Self) -> bool {
        self.node == other.node
    }
}

impl<T> Eq for ResolvedVc<T> where T: ?Sized {}

impl<T> Hash for ResolvedVc<T>
where
    T: ?Sized,
{
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.node.hash(state);
    }
}

impl<T, Inner, Repr> Default for ResolvedVc<T>
where
    T: VcValueType<Read = VcTransparentRead<T, Inner, Repr>>,
    Inner: Any + Send + Sync + Default,
    Repr: VcValueType,
{
    fn default() -> Self {
        Self::cell(Default::default())
    }
}

macro_rules! into_future {
    ($ty:ty) => {
        impl<T> IntoFuture for $ty
        where
            T: VcValueType,
        {
            type Output = <Vc<T> as IntoFuture>::Output;
            type IntoFuture = <Vc<T> as IntoFuture>::IntoFuture;
            fn into_future(self) -> Self::IntoFuture {
                (*self).into_future()
            }
        }
    };
}

into_future!(ResolvedVc<T>);
into_future!(&ResolvedVc<T>);
into_future!(&mut ResolvedVc<T>);

impl<T> ResolvedVc<T>
where
    T: VcValueType,
{
    // called by the `.resolved_cell()` method generated by the `#[turbo_tasks::value]` macro
    #[doc(hidden)]
    pub fn cell_private(inner: <T::Read as VcRead<T>>::Target) -> Self {
        Self {
            node: Vc::<T>::cell_private(inner),
        }
    }
}

impl<T, Inner, Repr> ResolvedVc<T>
where
    T: VcValueType<Read = VcTransparentRead<T, Inner, Repr>>,
    Inner: Any + Send + Sync,
    Repr: VcValueType,
{
    pub fn cell(inner: Inner) -> Self {
        Self {
            node: Vc::<T>::cell(inner),
        }
    }
}

impl<T> ResolvedVc<T>
where
    T: ?Sized,
{
    /// Upcasts the given `ResolvedVc<T>` to a `ResolvedVc<Box<dyn K>>`.
    ///
    /// See also: [`Vc::upcast`].
    #[inline(always)]
    pub fn upcast<K>(this: Self) -> ResolvedVc<K>
    where
        T: Upcast<K>,
        K: VcValueTrait + ?Sized,
    {
        ResolvedVc {
            node: Vc::upcast(this.node),
        }
    }
}

impl<T> ResolvedVc<T>
where
    T: VcValueTrait + ?Sized,
{
    /// Attempts to sidecast the given `Vc<Box<dyn T>>` to a `Vc<Box<dyn K>>`.
    ///
    /// Returns `None` if the underlying value type does not implement `K`.
    ///
    /// **Note:** if the trait `T` is required to implement `K`, use [`ResolvedVc::upcast`] instead.
    /// This provides stronger guarantees, removing the need for a [`Result`] return type.
    ///
    /// See also: [`Vc::try_resolve_sidecast`].
    pub async fn try_sidecast<K>(this: Self) -> Result<Option<ResolvedVc<K>>, ResolveTypeError>
    where
        K: VcValueTrait + ?Sized,
    {
        // TODO: Expose a synchronous API instead of this async one that returns `Result<Option<_>>`
        Ok(Self::try_sidecast_sync(this))
    }

    /// Attempts to sidecast the given `ResolvedVc<Box<dyn T>>` to a `ResolvedVc<Box<dyn K>>`.
    ///
    /// Returns `None` if the underlying value type does not implement `K`.
    ///
    /// **Note:** if the trait `T` is required to implement `K`, use [`ResolvedVc::upcast`] instead.
    /// This provides stronger guarantees, removing the need for a [`Result`] return type.
    ///
    /// See also: [`Vc::try_resolve_sidecast`].
    pub fn try_sidecast_sync<K>(this: Self) -> Option<ResolvedVc<K>>
    where
        K: VcValueTrait + ?Sized,
    {
        // `RawVc::TaskCell` already contains all the type information needed to check this
        // sidecast, so we don't need to read the underlying cell!
        let raw_vc = this.node.node;
        raw_vc
            .resolved_has_trait(<K as VcValueTrait>::get_trait_type_id())
            .then_some(ResolvedVc {
                node: Vc {
                    node: raw_vc,
                    _t: PhantomData,
                },
            })
    }

    /// Attempts to downcast the given `ResolvedVc<Box<dyn T>>` to a `ResolvedVc<K>`, where `K`
    /// is of the form `Box<dyn L>`, and `L` is a value trait.
    ///
    /// Returns `None` if the underlying value type is not a `K`.
    ///
    /// See also: [`Vc::try_resolve_downcast`].
    pub async fn try_downcast<K>(this: Self) -> Result<Option<ResolvedVc<K>>, ResolveTypeError>
    where
        K: Upcast<T> + VcValueTrait + ?Sized,
    {
        // TODO: Expose a synchronous API instead of this async one that returns `Result<Option<_>>`
        Ok(Self::try_downcast_sync(this))
    }

    /// Attempts to downcast the given `ResolvedVc<Box<dyn T>>` to a `ResolvedVc<K>`, where `K`
    /// is of the form `Box<dyn L>`, and `L` is a value trait.
    ///
    /// Returns `None` if the underlying value type is not a `K`.
    ///
    /// See also: [`Vc::try_resolve_downcast`].
    pub fn try_downcast_sync<K>(this: Self) -> Option<ResolvedVc<K>>
    where
        K: Upcast<T> + VcValueTrait + ?Sized,
    {
        // this is just a more type-safe version of a sidecast
        Self::try_sidecast_sync(this)
    }

    /// Attempts to downcast the given `Vc<Box<dyn T>>` to a `Vc<K>`, where `K` is a value type.
    ///
    /// Returns `None` if the underlying value type is not a `K`.
    ///
    /// See also: [`Vc::try_resolve_downcast_type`].
    pub async fn try_downcast_type<K>(this: Self) -> Result<Option<ResolvedVc<K>>, ResolveTypeError>
    where
        K: Upcast<T> + VcValueType,
    {
        // TODO: Expose a synchronous API instead of this async one that returns `Result<Option<_>>`
        Ok(Self::try_downcast_type_sync(this))
    }

    /// Attempts to downcast the given `Vc<Box<dyn T>>` to a `Vc<K>`, where `K` is a value type.
    ///
    /// Returns `None` if the underlying value type is not a `K`.
    ///
    /// See also: [`Vc::try_resolve_downcast_type`].
    pub fn try_downcast_type_sync<K>(this: Self) -> Option<ResolvedVc<K>>
    where
        K: Upcast<T> + VcValueType,
    {
        let raw_vc = this.node.node;
        raw_vc
            .resolved_is_type(<K as VcValueType>::get_value_type_id())
            .then_some(ResolvedVc {
                node: Vc {
                    node: raw_vc,
                    _t: PhantomData,
                },
            })
    }
}

/// Generates an opaque debug representation of the [`ResolvedVc`] itself, but not the data inside
/// of it.
///
/// This is implemented to allow types containing [`ResolvedVc`] to implement the synchronous
/// [`Debug`] trait, but in most cases users should use the [`ValueDebug`] implementation to get a
/// string representation of the contents of the cell.
impl<T> Debug for ResolvedVc<T>
where
    T: ?Sized,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedVc")
            .field("node", &self.node.node)
            .finish()
    }
}

impl<T> TraceRawVcs for ResolvedVc<T>
where
    T: ?Sized,
{
    fn trace_raw_vcs(&self, trace_context: &mut TraceRawVcsContext) {
        TraceRawVcs::trace_raw_vcs(&self.node, trace_context);
    }
}

impl<T> ValueDebugFormat for ResolvedVc<T>
where
    T: Upcast<Box<dyn ValueDebug>> + Send + Sync + ?Sized,
{
    fn value_debug_format(&self, depth: usize) -> ValueDebugFormatString {
        self.node.value_debug_format(depth)
    }
}
