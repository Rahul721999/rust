//! HIR datatypes. See the [rustc dev guide] for more info.
//!
//! [rustc dev guide]: https://rustc-dev-guide.rust-lang.org/hir.html

pub mod exports;
pub mod map;
pub mod place;

use crate::ty::query::Providers;
use crate::ty::TyCtxt;
use rustc_ast::Attribute;
use rustc_data_structures::fingerprint::Fingerprint;
use rustc_data_structures::fx::FxHashMap;
use rustc_data_structures::stable_hasher::{HashStable, StableHasher};
use rustc_hir::def_id::LocalDefId;
use rustc_hir::*;
use rustc_index::vec::{Idx, IndexVec};
use rustc_query_system::ich::StableHashingContext;
use rustc_span::DUMMY_SP;
use std::collections::BTreeMap;

/// Result of HIR indexing for a given HIR owner.
#[derive(Debug, HashStable)]
pub struct IndexedHir<'hir> {
    /// Contents of the HIR.
    nodes: OwnerNodes<'hir>,
    /// Map from each nested owner to its parent's local id.
    parenting: FxHashMap<LocalDefId, ItemLocalId>,
}

/// Top-level HIR node for current owner. This only contains the node for which
/// `HirId::local_id == 0`, and excludes bodies.
///
/// This struct exists to encapsulate all access to the hir_owner query in this module, and to
/// implement HashStable without hashing bodies.
#[derive(Copy, Clone, Debug)]
pub struct Owner<'tcx> {
    node: OwnerNode<'tcx>,
    node_hash: Fingerprint,
}

impl<'a, 'tcx> HashStable<StableHashingContext<'a>> for Owner<'tcx> {
    #[inline]
    fn hash_stable(&self, hcx: &mut StableHashingContext<'a>, hasher: &mut StableHasher) {
        let Owner { node: _, node_hash } = self;
        node_hash.hash_stable(hcx, hasher)
    }
}

/// HIR node coupled with its parent's id in the same HIR owner.
///
/// The parent is trash when the node is a HIR owner.
#[derive(Clone, Debug)]
pub struct ParentedNode<'tcx> {
    parent: ItemLocalId,
    node: Node<'tcx>,
}

#[derive(Debug)]
pub struct OwnerNodes<'tcx> {
    /// Pre-computed hash of the full HIR.
    hash: Fingerprint,
    /// Pre-computed hash of the top node.
    node_hash: Fingerprint,
    /// Full HIR for the current owner.
    // The zeroth node's parent is trash, but is never accessed.
    nodes: IndexVec<ItemLocalId, Option<ParentedNode<'tcx>>>,
    /// Content of local bodies.
    bodies: &'tcx IndexVec<ItemLocalId, Option<&'tcx Body<'tcx>>>,
}

impl<'a, 'tcx> HashStable<StableHashingContext<'a>> for OwnerNodes<'tcx> {
    #[inline]
    fn hash_stable(&self, hcx: &mut StableHashingContext<'a>, hasher: &mut StableHasher) {
        // We ignore the `nodes` and `bodies` fields since these refer to information included in
        // `hash` which is hashed in the collector and used for the crate hash.
        let OwnerNodes { hash, node_hash: _, nodes: _, bodies: _ } = *self;
        hash.hash_stable(hcx, hasher);
    }
}

/// Attributes owner by a HIR owner.
#[derive(Copy, Clone, Debug, HashStable)]
pub struct AttributeMap<'tcx> {
    map: &'tcx BTreeMap<ItemLocalId, &'tcx [Attribute]>,
}

impl<'tcx> AttributeMap<'tcx> {
    fn new(owner_info: &'tcx Option<OwnerInfo<'tcx>>) -> AttributeMap<'tcx> {
        const FALLBACK: &'static BTreeMap<ItemLocalId, &'static [Attribute]> = &BTreeMap::new();
        let map = owner_info.as_ref().map_or(FALLBACK, |info| &info.attrs);
        AttributeMap { map }
    }

    fn get(&self, id: ItemLocalId) -> &'tcx [Attribute] {
        self.map.get(&id).copied().unwrap_or(&[])
    }
}

/// Gather the LocalDefId for each item-like within a module, including items contained within
/// bodies.  The Ids are in visitor order.  This is used to partition a pass between modules.
#[derive(Debug, HashStable)]
pub struct ModuleItems {
    submodules: Box<[LocalDefId]>,
    items: Box<[ItemId]>,
    trait_items: Box<[TraitItemId]>,
    impl_items: Box<[ImplItemId]>,
    foreign_items: Box<[ForeignItemId]>,
}

impl<'tcx> TyCtxt<'tcx> {
    #[inline(always)]
    pub fn hir(self) -> map::Map<'tcx> {
        map::Map { tcx: self }
    }

    pub fn parent_module(self, id: HirId) -> LocalDefId {
        self.parent_module_from_def_id(id.owner)
    }
}

pub fn provide(providers: &mut Providers) {
    providers.parent_module_from_def_id = |tcx, id| {
        let hir = tcx.hir();
        hir.local_def_id(hir.get_module_parent_node(hir.local_def_id_to_hir_id(id)))
    };
    providers.hir_crate = |tcx, ()| tcx.untracked_crate;
    providers.index_hir = map::index_hir;
    providers.crate_hash = map::crate_hash;
    providers.hir_module_items = map::hir_module_items;
    providers.hir_owner = |tcx, id| {
        let owner = tcx.index_hir(id)?;
        let node = owner.nodes.nodes[ItemLocalId::new(0)].as_ref().unwrap().node;
        let node = node.as_owner().unwrap(); // Indexing must ensure it is an OwnerNode.
        Some(Owner { node, node_hash: owner.nodes.node_hash })
    };
    providers.hir_owner_nodes = |tcx, id| tcx.index_hir(id).map(|i| &i.nodes);
    providers.hir_owner_parent = |tcx, id| {
        let parent = tcx.untracked_resolutions.definitions.def_key(id).parent;
        let parent = parent.map_or(CRATE_HIR_ID, |local_def_index| {
            let def_id = LocalDefId { local_def_index };
            let mut parent_hir_id =
                tcx.untracked_resolutions.definitions.local_def_id_to_hir_id(def_id);
            if let Some(local_id) = tcx.index_hir(parent_hir_id.owner).unwrap().parenting.get(&id) {
                parent_hir_id.local_id = *local_id;
            }
            parent_hir_id
        });
        parent
    };
    providers.hir_attrs = |tcx, id| AttributeMap::new(&tcx.hir_crate(()).owners[id]);
    providers.source_span = |tcx, def_id| tcx.resolutions(()).definitions.def_span(def_id);
    providers.def_span = |tcx, def_id| tcx.hir().span_if_local(def_id).unwrap_or(DUMMY_SP);
    providers.fn_arg_names = |tcx, id| {
        let hir = tcx.hir();
        let hir_id = hir.local_def_id_to_hir_id(id.expect_local());
        if let Some(body_id) = hir.maybe_body_owned_by(hir_id) {
            tcx.arena.alloc_from_iter(hir.body_param_names(body_id))
        } else if let Node::TraitItem(&TraitItem {
            kind: TraitItemKind::Fn(_, TraitFn::Required(idents)),
            ..
        }) = hir.get(hir_id)
        {
            tcx.arena.alloc_slice(idents)
        } else {
            span_bug!(hir.span(hir_id), "fn_arg_names: unexpected item {:?}", id);
        }
    };
    providers.opt_def_kind = |tcx, def_id| tcx.hir().opt_def_kind(def_id.expect_local());
    providers.all_local_trait_impls = |tcx, ()| &tcx.resolutions(()).trait_impls;
    providers.expn_that_defined = |tcx, id| {
        let id = id.expect_local();
        tcx.resolutions(()).definitions.expansion_that_defined(id)
    };
}
