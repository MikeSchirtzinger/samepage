//! Declared extension dependencies, host service requirements, and shared
//! document ownership.
//!
//! The [`Extension`](crate::Extension) trait already made state, actions,
//! routes and browser modules travel together. What it could not express is the
//! part an application supplied by hand: an `Arc` passed from `main` into one
//! extension's constructor, a second extension writing a document a third one
//! owns, and a capability string with no provider behind it.
//!
//! Each of those is a real dependency that the recipe did not mention, so a
//! reviewer reading `agui.app.toml` saw an application made of independent
//! parts when it was not. This module makes the dependency declarable, checks
//! the declaration against compiled code, and refuses to start when a declared
//! requirement has no provider.
//!
//! ## What is enforced here
//!
//! - A required host service must resolve to a real injected value, of the type
//!   the extension asks for, or composition fails before the socket binds.
//!   A capability string alone is metadata; this is the provider.
//! - An extension dependency must resolve to an enabled extension at the pinned
//!   version, must not be circular, and must be listed before its dependent so
//!   the recipe reads in dependency order.
//! - A shared document has exactly one owner. An extension that writes a
//!   document it does not own must declare a dependency on the owner, which is
//!   the check `same-page-readiness` would have failed while it wrote
//!   `understanding-projection`'s cards.
//!
//! ## What is not
//!
//! Nothing here makes an undeclared `Arc` impossible. An application can still
//! hand a value to a constructor without registering it. What changes is that
//! doing so is now a choice against an available declaration rather than the
//! only mechanism, and an extension published with `requires_services` cannot
//! be composed by an app that has not supplied them.

use std::any::{type_name, Any};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use parking_lot::{Mutex, MutexGuard};

/// A named host service an extension declares it cannot run without.
///
/// `reason` is not decoration. It is read by a person deciding whether to grant
/// the service, and a requirement that cannot explain itself is one an
/// extension author has not thought about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRequirement {
    /// Registry key, matched exactly against [`ServiceRegistry::provide`].
    pub service: &'static str,
    /// Why this extension needs it, in one reviewable sentence.
    pub reason: &'static str,
}

impl ServiceRequirement {
    pub const fn new(service: &'static str, reason: &'static str) -> Self {
        Self { service, reason }
    }
}

/// Another extension this one reads or writes through.
///
/// Declaring the dependency is what lets composition order the recipe, reject
/// a cycle, and refuse a configuration where the dependency was disabled while
/// the dependent stayed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionDependency {
    /// The depended-on extension's id.
    pub extension: &'static str,
    /// The exact version expected. Composition compares this against the
    /// compiled extension, not against the recipe pin, so a recipe cannot
    /// satisfy a dependency the code would not accept.
    pub version: &'static str,
    /// Why the dependency exists, in one reviewable sentence.
    pub reason: &'static str,
}

impl ExtensionDependency {
    pub const fn new(extension: &'static str, version: &'static str, reason: &'static str) -> Self {
        Self {
            extension,
            version,
            reason,
        }
    }
}

/// A named document with exactly one authoritative owner.
///
/// Ownership is declared separately from writing on purpose. Two extensions
/// projecting the same document is normal; two extensions *owning* it means
/// neither can state the revision, which is the condition the extension catalog
/// audit found in both the Studio and site-review families.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentClaim {
    /// Document name, unique across the composition.
    pub document: &'static str,
    /// What this extension does with it, in one reviewable sentence.
    pub reason: &'static str,
}

impl DocumentClaim {
    pub const fn new(document: &'static str, reason: &'static str) -> Self {
        Self { document, reason }
    }
}

/// Host services an application injects at startup, keyed by name.
///
/// Type-erased so the crate does not need to know an application's concrete
/// service types, but [`resolve`](Self::resolve) is typed: a name registered
/// with the wrong type fails loudly rather than resolving to `None` and letting
/// an extension decide the service was simply absent.
#[derive(Default)]
pub struct ServiceRegistry {
    services: BTreeMap<String, Arc<dyn Any + Send + Sync>>,
}

impl ServiceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one service. Rejects a duplicate name rather than replacing it:
    /// a silent overwrite would let load order decide which implementation an
    /// extension receives.
    pub fn provide<T>(&mut self, name: impl Into<String>, service: Arc<T>) -> Result<(), String>
    where
        T: Send + Sync + 'static,
    {
        let name = name.into();
        if name.trim().is_empty() {
            return Err("a host service name cannot be empty".to_string());
        }
        if self.services.contains_key(&name) {
            return Err(format!(
                "host service {name:?} is already provided; registering it twice would let \
                 load order decide which implementation extensions receive"
            ));
        }
        self.services.insert(name, service);
        Ok(())
    }

    /// Whether a name has a provider, without caring about its type. Used by
    /// composition to report every missing service at once.
    pub fn provides(&self, name: &str) -> bool {
        self.services.contains_key(name)
    }

    /// Resolve a service to its concrete type.
    ///
    /// The two failure modes are reported separately because they have
    /// different fixes: an absent service is an application that did not
    /// provide it, a mistyped one is an application that provided something
    /// else under the same name.
    pub fn resolve<T>(&self, name: &str) -> Result<Arc<T>, String>
    where
        T: Send + Sync + 'static,
    {
        let service = self.services.get(name).ok_or_else(|| {
            format!(
                "host service {name:?} was not provided (available: {:?})",
                self.names()
            )
        })?;
        service.clone().downcast::<T>().map_err(|_| {
            format!(
                "host service {name:?} was provided, but not as {}",
                type_name::<T>()
            )
        })
    }

    /// Every registered name, sorted, for error messages and diagnostics.
    pub fn names(&self) -> Vec<&str> {
        self.services.keys().map(String::as_str).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.services.is_empty()
    }
}

impl std::fmt::Debug for ServiceRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServiceRegistry")
            .field("services", &self.names())
            .finish()
    }
}

thread_local! {
    /// Documents this thread currently holds open in a transaction, so a
    /// staging closure that re-enters the same document reports a wiring bug
    /// instead of deadlocking a server thread on a non-reentrant lock.
    static HELD: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// One shared document: a declared owner, a gate that serializes writers, and a
/// revision that advances exactly once per committed transaction.
///
/// The document's *contents* live wherever the owning extension keeps them.
/// This type owns the two things a cross-extension write needs and neither
/// extension can provide alone — mutual exclusion and an agreed revision.
pub struct SharedDocument {
    name: String,
    owner: String,
    revision: Mutex<u64>,
}

impl SharedDocument {
    fn new(name: impl Into<String>, owner: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            owner: owner.into(),
            revision: Mutex::new(0),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The extension that declared [`Extension::owns_documents`](crate::Extension::owns_documents)
    /// for this name. Composition has already proven there is exactly one.
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// The revision as of the last committed transaction. Sampling it outside a
    /// transaction is a read of the past, not a reservation of the future.
    pub fn revision(&self) -> u64 {
        *self.revision.lock()
    }

    /// Open a transaction, blocking until any other writer commits or aborts.
    ///
    /// Fails instead of deadlocking when this thread already holds the
    /// document. That case is always a wiring bug — a staging closure reaching
    /// back into the document it is staging against — and a hung server thread
    /// is a far worse way to learn about it than an error.
    pub fn transact(&self) -> Result<Transaction<'_>, String> {
        let reentrant = HELD.with(|held| !held.borrow_mut().insert(self.name.clone()));
        if reentrant {
            return Err(format!(
                "document {:?} is already open in a transaction on this thread; \
                 staging must not re-enter the document it is staging against",
                self.name
            ));
        }
        Ok(Transaction {
            document: self,
            revision: self.revision.lock(),
            staged: Vec::new(),
            rejected: None,
        })
    }
}

impl std::fmt::Debug for SharedDocument {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SharedDocument")
            .field("name", &self.name)
            .field("owner", &self.owner)
            .field("revision", &self.revision())
            .finish()
    }
}

/// An exclusive, all-or-nothing write across every extension that participates
/// in one shared document.
///
/// ## The guarantee, and its edge
///
/// Participants **stage** rather than write. Staging validates and computes,
/// and returns a closure that performs the write. The host applies no closure
/// until every participant has staged successfully, so a rejection by the last
/// participant leaves the first participant's changes unmade — not rolled back,
/// never made. One revision advance covers the whole set.
///
/// What the host cannot do is stop a participant from mutating its own state
/// *during* staging, because interior mutability makes that reachable from any
/// `&self`. The type is shaped to make the correct thing the easy thing —
/// staging returns work rather than doing it — but a participant that ignores
/// that gets no atomicity from this type, and no host-side undo exists to save
/// it. The guarantee is precisely: **correct participants never observe a
/// composition where some staged writes landed and others were rejected.**
pub struct Transaction<'a> {
    document: &'a SharedDocument,
    revision: MutexGuard<'a, u64>,
    staged: Vec<(String, Box<dyn FnOnce() + Send>)>,
    rejected: Option<(String, String)>,
}

impl<'a> Transaction<'a> {
    pub fn document(&self) -> &SharedDocument {
        self.document
    }

    /// The revision this transaction will produce if it commits. Available
    /// during staging so a participant can stamp its own write with the
    /// revision the whole set will share.
    pub fn pending_revision(&self) -> u64 {
        *self.revision + 1
    }

    /// Stage one participant's write.
    ///
    /// `stage` runs now: it validates, computes, and returns the closure that
    /// applies the result. Returning `Err` rejects the whole transaction. Once
    /// any participant has been rejected, later `stage` calls do not run their
    /// closures at all — the transaction is already lost, and running further
    /// validation would only risk side effects on a write that will not happen.
    pub fn stage<F>(
        &mut self,
        extension: impl Into<String>,
        stage: impl FnOnce() -> Result<F, String>,
    ) where
        F: FnOnce() + Send + 'static,
    {
        if self.rejected.is_some() {
            return;
        }
        let extension = extension.into();
        match stage() {
            Ok(apply) => self.staged.push((extension, Box::new(apply))),
            Err(error) => self.rejected = Some((extension, error)),
        }
    }

    /// Apply every staged write and advance the revision once.
    ///
    /// Returns the rejection with nothing applied if any participant failed to
    /// stage, and names which one, because "the transaction failed" is not
    /// actionable when four extensions participate.
    pub fn commit(mut self) -> Result<u64, String> {
        if let Some((extension, error)) = self.rejected.take() {
            return Err(format!(
                "transaction on document {:?} was rejected by {extension}: {error}; \
                 no participant's write was applied",
                self.document.name
            ));
        }
        for (_, apply) in std::mem::take(&mut self.staged) {
            apply();
        }
        *self.revision += 1;
        Ok(*self.revision)
    }

    /// Participants staged so far, in staging order. For diagnostics and tests.
    pub fn participants(&self) -> Vec<&str> {
        self.staged
            .iter()
            .map(|(extension, _)| extension.as_str())
            .collect()
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        // Whether this transaction committed, was rejected, or was simply
        // dropped, the thread no longer holds the document. Anything staged and
        // not committed is discarded here, which is the abort path.
        HELD.with(|held| {
            held.borrow_mut().remove(&self.document.name);
        });
    }
}

/// Every shared document in one composition, built by composition from the
/// ownership declarations it has already validated.
///
/// Extensions receive this through
/// [`Extension::bind_documents`](crate::Extension::bind_documents). They do not
/// construct it: a document an extension invented for itself would have no
/// validated owner, which is the property the registry exists to carry.
#[derive(Default, Debug)]
pub struct DocumentRegistry {
    documents: BTreeMap<String, Arc<SharedDocument>>,
}

impl DocumentRegistry {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Register one validated ownership claim. Crate-private: composition is
    /// the only caller, after it has proven the name has exactly one owner.
    pub(crate) fn declare(&mut self, document: &str, owner: &str) {
        self.documents.insert(
            document.to_string(),
            Arc::new(SharedDocument::new(document, owner)),
        );
    }

    /// Look up a document by name.
    ///
    /// Composition has already refused any composition in which an extension
    /// writes a document nobody owns, so a bound extension asking for a
    /// document it declared will find it.
    pub fn get(&self, document: &str) -> Result<Arc<SharedDocument>, String> {
        self.documents.get(document).cloned().ok_or_else(|| {
            format!(
                "no composed extension owns document {document:?} (composed: {:?})",
                self.names()
            )
        })
    }

    pub fn names(&self) -> Vec<&str> {
        self.documents.keys().map(String::as_str).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Repo(&'static str);
    #[derive(Debug)]
    struct NotARepo;

    #[test]
    fn a_service_resolves_to_its_concrete_type() {
        let mut registry = ServiceRegistry::new();
        registry
            .provide("repository", Arc::new(Repo("/workspace")))
            .expect("provide the repository service");

        assert!(registry.provides("repository"));
        let repo = registry
            .resolve::<Repo>("repository")
            .expect("resolve the repository service");
        assert_eq!(repo.0, "/workspace");
    }

    #[test]
    fn absent_and_mistyped_services_fail_differently() {
        let mut registry = ServiceRegistry::new();
        registry
            .provide("repository", Arc::new(Repo("/workspace")))
            .expect("provide the repository service");

        let absent = registry
            .resolve::<Repo>("telemetry")
            .expect_err("an unprovided service must not resolve");
        assert!(absent.contains("was not provided"), "{absent}");
        assert!(absent.contains("repository"), "{absent}");

        let mistyped = registry
            .resolve::<NotARepo>("repository")
            .expect_err("a service provided under another type must not resolve");
        assert!(mistyped.contains("but not as"), "{mistyped}");
    }

    /// Stand-in for two extensions' state sharing one document.
    #[derive(Default)]
    struct Views {
        cards: Mutex<Vec<String>>,
        approvals: Mutex<Vec<String>>,
    }

    fn document(name: &str, owner: &str) -> Arc<SharedDocument> {
        let mut registry = DocumentRegistry::new();
        registry.declare(name, owner);
        registry.get(name).expect("the declared document resolves")
    }

    #[test]
    fn a_committed_transaction_applies_every_view_and_advances_once() {
        let document = document("studio", "projection");
        let views = Arc::new(Views::default());
        assert_eq!(document.revision(), 0);

        let mut transaction = document.transact().expect("open a transaction");
        let stamped = transaction.pending_revision();
        transaction.stage("projection", || {
            let views = views.clone();
            Ok(move || views.cards.lock().push(format!("card@{stamped}")))
        });
        transaction.stage("alignment", || {
            let views = views.clone();
            Ok(move || views.approvals.lock().push(format!("approved@{stamped}")))
        });
        assert_eq!(transaction.participants(), vec!["projection", "alignment"]);
        assert_eq!(transaction.commit().expect("commit"), 1);

        assert_eq!(*views.cards.lock(), vec!["card@1".to_string()]);
        assert_eq!(*views.approvals.lock(), vec!["approved@1".to_string()]);
        // One revision for the whole set, not one per participant.
        assert_eq!(document.revision(), 1);
    }

    #[test]
    fn a_rejection_by_any_view_leaves_no_write_behind() {
        let document = document("studio", "projection");
        let views = Arc::new(Views::default());

        let mut transaction = document.transact().expect("open a transaction");
        transaction.stage("projection", || {
            let views = views.clone();
            Ok(move || views.cards.lock().push("card".to_string()))
        });
        // The second participant rejects. The first already staged.
        transaction.stage("alignment", || {
            Err::<fn(), _>("the projection revision moved under me".to_string())
        });
        let error = transaction
            .commit()
            .expect_err("a rejected transaction must not commit");

        assert!(error.contains("rejected by alignment"), "{error}");
        assert!(
            error.contains("no participant's write was applied"),
            "{error}"
        );
        assert!(
            views.cards.lock().is_empty(),
            "the first participant's write must never have been made: {:?}",
            views.cards.lock()
        );
        assert_eq!(
            document.revision(),
            0,
            "a rejected transaction must not advance the revision"
        );
    }

    #[test]
    fn staging_stops_once_a_participant_has_rejected() {
        let document = document("studio", "projection");
        let ran = Arc::new(Mutex::new(Vec::<&str>::new()));

        let mut transaction = document.transact().expect("open a transaction");
        transaction.stage("first", || {
            ran.lock().push("first");
            Err::<fn(), _>("no".to_string())
        });
        transaction.stage("second", || {
            ran.lock().push("second");
            Ok(|| {})
        });
        let _ = transaction.commit();

        assert_eq!(
            *ran.lock(),
            vec!["first"],
            "staging after a rejection risks side effects for a write that cannot happen"
        );
    }

    #[test]
    fn a_dropped_transaction_aborts_and_releases_the_document() {
        let document = document("studio", "projection");
        let views = Arc::new(Views::default());
        {
            let mut transaction = document.transact().expect("open a transaction");
            transaction.stage("projection", || {
                let views = views.clone();
                Ok(move || views.cards.lock().push("card".to_string()))
            });
            // Dropped without commit.
        }
        assert!(
            views.cards.lock().is_empty(),
            "an abandoned transaction must apply nothing"
        );
        assert_eq!(document.revision(), 0);
        // The gate is free again, not poisoned by the abort.
        document
            .transact()
            .expect("a dropped transaction must release the document")
            .commit()
            .expect("commit");
        assert_eq!(document.revision(), 1);
    }

    #[test]
    fn concurrent_writers_serialize_instead_of_losing_updates() {
        // Each writer reads the shared total during staging and writes
        // `observed + 1` at commit. Read and write are separate lock
        // acquisitions, so the only thing preventing a lost update is the
        // document gate being held across both.
        const WRITERS: u64 = 16;

        // Negative control first, so the real assertion below means something.
        // Same read-then-write, no document, and a barrier that forces every
        // thread to read before any thread writes. Deterministic rather than
        // timing-dependent: all sixteen read 0, all sixteen write 1.
        let ungated = Arc::new(Mutex::new(0u64));
        let barrier = Arc::new(std::sync::Barrier::new(WRITERS as usize));
        std::thread::scope(|scope| {
            for _ in 0..WRITERS {
                let ungated = ungated.clone();
                let barrier = barrier.clone();
                scope.spawn(move || {
                    let observed = *ungated.lock();
                    barrier.wait();
                    *ungated.lock() = observed + 1;
                });
            }
        });
        assert_eq!(
            *ungated.lock(),
            1,
            "the control must lose updates, or the gated case below proves nothing"
        );

        // The same pattern through a transaction. No barrier: threads holding
        // the gate cannot wait for each other by construction, which is the
        // property under test.
        let document = document("studio", "projection");
        let total = Arc::new(Mutex::new(0u64));
        std::thread::scope(|scope| {
            for _ in 0..WRITERS {
                let document = document.clone();
                let total = total.clone();
                scope.spawn(move || {
                    let mut transaction = document.transact().expect("open a transaction");
                    transaction.stage("writer", || {
                        let observed = *total.lock();
                        let total = total.clone();
                        Ok(move || *total.lock() = observed + 1)
                    });
                    transaction.commit().expect("commit");
                });
            }
        });

        assert_eq!(
            *total.lock(),
            WRITERS,
            "a lost update means the gate did not hold across stage and commit"
        );
        assert_eq!(document.revision(), WRITERS);
    }

    #[test]
    fn re_entering_a_document_reports_a_bug_instead_of_deadlocking() {
        // If this regressed, the test would hang rather than fail — which is
        // exactly the server behavior the check exists to prevent.
        let document = document("studio", "projection");
        let outer = document.transact().expect("open a transaction");
        let error = document
            .transact()
            .err()
            .expect("re-entering the same document on one thread must not deadlock");
        assert!(error.contains("already open in a transaction"), "{error}");

        drop(outer);
        document
            .transact()
            .expect("the document is available again once the first transaction ends");
    }

    #[test]
    fn an_undeclared_document_does_not_resolve() {
        let mut registry = DocumentRegistry::new();
        registry.declare("studio", "projection");
        let error = registry
            .get("review")
            .expect_err("a document nobody owns must not resolve");
        assert!(error.contains("no composed extension owns"), "{error}");
        assert!(error.contains("studio"), "{error}");
    }

    #[test]
    fn providing_a_name_twice_is_refused_rather_than_overwritten() {
        let mut registry = ServiceRegistry::new();
        registry
            .provide("repository", Arc::new(Repo("/first")))
            .expect("provide the repository service");
        let error = registry
            .provide("repository", Arc::new(Repo("/second")))
            .expect_err("a duplicate service name must be refused");
        assert!(error.contains("already provided"), "{error}");

        // The first provider survives; the duplicate did not silently win.
        assert_eq!(
            registry
                .resolve::<Repo>("repository")
                .expect("resolve the repository service")
                .0,
            "/first"
        );
    }
}
