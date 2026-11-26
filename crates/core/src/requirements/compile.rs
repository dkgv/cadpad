//! The whole compiler on one requirement (spec §2): bind, check provenance,
//! typecheck, coverage, expand, link, plan, print. Pure: the build snapshot
//! and settings come in, everything else comes out.
use super::assembly::{assembly_report, verify_covers};
use super::bind::{Bindings, bind};
use super::coverage::{check_coverage, check_vague};
use super::diagnostics::Diagnostic;
use super::expand::{PrimitiveClaim, expand_requirement};
use super::ir::Requirement;
use super::manifest::{Manifest, link};
use super::plan::{Plan, plan, unused_allowances};
use super::print::{Print, print_requirement};
use super::provenance::{Downgrade, check_provenance};
use super::settings::ProjectSettings;
use super::snapshot::WorkspaceSnapshot;
use super::typecheck::typecheck;
use serde::Serialize;

/// Content hashes of what the user approves (P1.43).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Hashes {
    pub requirement: String,
    pub manifest: String,
    pub plan: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Compiled {
    /// The IR after provenance downgrades: this is what is hashed and printed.
    pub requirement: Requirement,
    pub diagnostics: Vec<Diagnostic>,
    pub downgrades: Vec<Downgrade>,
    pub bindings: Bindings,
    pub primitives: Vec<PrimitiveClaim>,
    /// Present only when there are no errors.
    pub manifest: Option<Manifest>,
    pub plan: Option<Plan>,
    pub print: Option<Print>,
    pub hashes: Option<Hashes>,
}

impl Compiled {
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(Diagnostic::is_error)
    }
}

/// Compile one requirement against a workspace snapshot.
pub fn compile(
    req: &Requirement,
    snap: &WorkspaceSnapshot,
    settings: &ProjectSettings,
) -> Compiled {
    let (bindings, mut diagnostics) = bind(req, snap);
    let (requirement, downgrades) = check_provenance(req, &bindings, snap, settings);
    diagnostics.extend(settings.validate());
    diagnostics.extend(typecheck(&requirement, &bindings, snap, settings));
    diagnostics.extend(check_vague(&requirement));
    diagnostics.extend(check_coverage(&requirement, &bindings));
    // Assemblies (spec §3.2): chip mappings are verified against the build,
    // and placements the claims rely on are re-checked as inferred seating.
    diagnostics.extend(verify_covers(&requirement, &bindings, snap));
    let assembly = assembly_report(&requirement, &bindings, snap);
    diagnostics.extend(assembly.diagnostics.clone());
    // A diagnostic can come from two stages (e.g. a missing set): keep one.
    let mut seen = std::collections::BTreeSet::new();
    diagnostics.retain(|d| seen.insert((d.code, d.at.clone(), d.message.clone())));

    let mut out = Compiled {
        requirement,
        diagnostics,
        downgrades,
        bindings,
        primitives: vec![],
        manifest: None,
        plan: None,
        print: None,
        hashes: None,
    };
    if out.has_errors() {
        return out;
    }
    // Compiler-added claims are part of the IR the user reviews and approves
    // (spec §2.2 "automate and hide; never unrecord").
    out.requirement.claims.extend(assembly.claims);
    match expand_requirement(&out.requirement, settings) {
        Ok(p) => out.primitives = p,
        Err(e) => {
            out.diagnostics.extend(e);
            return out;
        }
    }
    let manifest = link(
        &out.requirement,
        &out.bindings,
        &out.primitives,
        settings,
        snap,
    );
    match plan(&out.requirement, &out.primitives, &out.bindings, &manifest) {
        Ok(p) => {
            out.diagnostics
                .extend(unused_allowances(&out.requirement, &p));
            out.print = Some(print_requirement(
                &out.requirement,
                &out.bindings,
                &out.primitives,
                Some(&p),
                settings,
            ));
            out.hashes = Some(Hashes {
                requirement: out.requirement.content_hash(),
                manifest: manifest.content_hash(),
                plan: p.content_hash(),
            });
            out.plan = Some(p);
            out.manifest = Some(manifest);
        }
        Err(e) => out.diagnostics.extend(e),
    }
    out
}
