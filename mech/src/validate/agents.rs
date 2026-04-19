//! Agent validation methods on [`Validator`].

use std::collections::BTreeSet;

use crate::schema::{AgentConfig, AgentConfigRef};

use super::Validator;
use super::helpers::VALID_GRANTS;
use super::model::ModelChecker;
use super::report::{Location, ValidationIssue};

/// Compute the normalized grant set for an agent config. `write` and
/// `network` imply `tools`; a non-empty tools list also implies `tools`.
fn expanded_grants(ac: &AgentConfig) -> BTreeSet<String> {
    let mut set: BTreeSet<String> = ac.grants_list().iter().cloned().collect();
    if set.contains("write") || set.contains("network") {
        set.insert("tools".to_string());
    }
    if !ac.tool_list().is_empty() {
        set.insert("tools".to_string());
    }
    set
}

impl Validator<'_> {
    /// Validate the named-agents map from `workflow.agents`.
    pub(crate) fn validate_named_agents(
        &mut self,
        defaults: &crate::schema::WorkflowSection,
        models: &dyn ModelChecker,
    ) {
        // Per-agent validity
        for (name, ac) in &defaults.agents {
            let loc = self
                .root_loc()
                .with_field(format!("workflow.agents.{name}"));
            if ac.extends.is_some() {
                self.err(
                    loc.clone().with_field("extends"),
                    format!(
                        "named agent `{name}` must not use `extends` (extends is only permitted on inline agent configs)"
                    ),
                );
            }
            self.validate_agent_inline(ac, models, loc);
        }

        // Defense-in-depth: cycle detection on extends chains.
        // Under current rules, named agents with `extends` are rejected above
        // (line 24-28), so this walk cannot discover new cycles that weren't
        // already flagged. It is kept as a safety net in case `extends` is
        // allowed on named agents in the future.
        // Deduplicate missing extends target reports so each missing
        // name is flagged only once across the whole walk.
        let mut reported_missing: BTreeSet<String> = BTreeSet::new();
        for name in defaults.agents.keys() {
            let mut seen: BTreeSet<String> = BTreeSet::new();
            let mut cur = Some(name.clone());
            while let Some(c) = cur {
                if !seen.insert(c.clone()) {
                    self.err(
                        self.root_loc()
                            .with_field(format!("workflow.agents.{name}.extends")),
                        format!("cyclic `extends` chain involving agent `{c}`"),
                    );
                    break;
                }
                cur = defaults
                    .agents
                    .get(&c)
                    .and_then(|a| a.extends.clone())
                    .filter(|n| {
                        if !defaults.agents.contains_key(n) {
                            if reported_missing.insert(n.clone()) {
                                self.report.errors.push(ValidationIssue::new(
                                    Location::root(self.file)
                                        .with_field(format!("workflow.agents.{c}.extends")),
                                    format!("`extends` target `{n}` is not a named agent"),
                                ));
                            }
                            false
                        } else {
                            true
                        }
                    });
            }
        }
    }

    pub(crate) fn validate_agent_inline(
        &mut self,
        ac: &AgentConfig,
        models: &dyn ModelChecker,
        loc: Location,
    ) {
        if let Some(model) = &ac.model
            && !model.is_empty()
            && !models.is_known(model)
        {
            self.err(
                loc.clone().with_field("model"),
                format!("agent model `{model}` is not known to the model registry"),
            );
        }
        for g in ac.grants_list() {
            if !VALID_GRANTS.contains(&g.as_str()) {
                self.err(
                    loc.clone().with_field("grant"),
                    format!("invalid grant `{g}`; must be one of tools/write/network"),
                );
            }
        }
        let normalized = expanded_grants(ac);
        if !ac.write_path_list().is_empty() && !normalized.contains("write") {
            self.warn(
                loc.with_field("write_paths"),
                "`write_paths` is set but `write` grant is not present (write_paths will be ignored)",
            );
        }
    }

    /// Validate an agent reference that requires `WorkflowSection`.
    ///
    /// This strict form requires `workflow.agents` defaults to be present;
    /// callers without defaults should use [`Self::validate_agent_ref`].
    pub(crate) fn validate_agent_ref_strict(
        &mut self,
        agent_ref: &AgentConfigRef,
        defaults: &crate::schema::WorkflowSection,
        models: &dyn ModelChecker,
        loc: Location,
    ) {
        match agent_ref {
            AgentConfigRef::Inline(ac) => {
                if let Some(parent) = &ac.extends
                    && !defaults.agents.contains_key(parent)
                {
                    self.err(
                        loc.clone().with_field("extends"),
                        format!("`extends` target `{parent}` is not a named agent"),
                    );
                }
                self.validate_agent_inline(ac, models, loc);
            }
            AgentConfigRef::Ref(raw) => {
                // Route through the canonical parser so malformed/unsupported
                // are distinguished consistently across crate boundaries.
                match crate::schema::parse_named_ref(raw) {
                    Ok(name) => {
                        if !defaults.agents.contains_key(name) {
                            self.err(
                                loc,
                                format!(
                                    "agent $ref `#{name}` is not a named agent in `workflow.agents`"
                                ),
                            );
                        }
                    }
                    Err(crate::error::MechError::SchemaRefUnsupported { .. }) => {
                        self.err(
                            loc,
                            format!("external file agent $ref `{raw}` is not supported; only `$ref:#name` references are allowed"),
                        );
                    }
                    Err(crate::error::MechError::SchemaRefMalformed { .. }) => {
                        self.err(loc, format!("malformed agent $ref: `{raw}`"));
                    }
                    Err(_) => unreachable!(
                        "parse_named_ref returns only SchemaRefMalformed/SchemaRefUnsupported"
                    ),
                }
            }
        }
    }

    /// Validate an agent reference with optional defaults.
    ///
    /// General-purpose form: dispatches to [`Self::validate_agent_ref_strict`]
    /// when defaults are present, otherwise rejects `extends` and named
    /// `$ref` because there are no `workflow.agents` to resolve against.
    pub(crate) fn validate_agent_ref(
        &mut self,
        agent_ref: &AgentConfigRef,
        defaults: Option<&crate::schema::WorkflowSection>,
        models: &dyn ModelChecker,
        loc: Location,
    ) {
        if let Some(d) = defaults {
            self.validate_agent_ref_strict(agent_ref, d, models, loc);
        } else {
            match agent_ref {
                AgentConfigRef::Inline(ac) => {
                    if let Some(parent) = &ac.extends {
                        self.err(
                            loc.clone().with_field("extends"),
                            format!(
                                "`extends` target `{parent}` is not a named agent (no `workflow.agents` declared)"
                            ),
                        );
                    }
                    self.validate_agent_inline(ac, models, loc);
                }
                AgentConfigRef::Ref(raw) => {
                    self.err(
                        loc,
                        format!("agent $ref `{raw}` cannot resolve: no `workflow.agents` declared"),
                    );
                }
            }
        }
    }
}
