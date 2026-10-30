use anyhow::{bail, ensure, Context, Result};
use toml::Value;

use crate::scenario::Scenario;
use crate::trace::Sha256Digest;

pub fn derive(base_text: &str, declarations: &str) -> Result<Vec<(String, String)>> {
    let base = Scenario::parse(base_text)?;
    let manifest: Value = declarations.parse().context("variant declarations")?;
    ensure!(
        manifest.as_table().is_some_and(|table| table.len() == 1),
        "variant declarations may contain only variants"
    );
    let variants = manifest
        .get("variants")
        .and_then(Value::as_array)
        .context("variant declarations need an array")?;
    ensure!(!variants.is_empty(), "variant declarations are empty");
    let mut derived = Vec::new();
    for declaration in variants {
        let fields = declaration.as_table().context("variant must be a table")?;
        let axis = declaration
            .get("axis")
            .and_then(Value::as_str)
            .context("variant needs an axis")?;
        ensure!(
            matches!(
                axis,
                "observer"
                    | "late_observer"
                    | "reconnect"
                    | "opposite_faction"
                    | "spawn_beside_viewer"
            ),
            "unknown variant axis {axis}"
        );
        let allowed: &[&str] = match axis {
            "observer" | "spawn_beside_viewer" => &["axis", "viewer", "scope_packets"],
            "late_observer" => &[
                "axis",
                "viewer",
                "step",
                "after_ms",
                "watcher_movement",
                "viewer_broadcasts",
                "scope_packets",
                "exclude_self_for",
                "identities",
                "tolerances",
                "clocks",
                "packet_timing",
                "unexpected_packets",
            ],
            "reconnect" => &[
                "axis",
                "step",
                "after_ms",
                "identities",
                "clocks",
                "tolerances",
                "replies",
                "values_only_assertions",
                "exclude_create_values",
                "data_fields",
                "packet_timing",
                "unexpected_packets",
            ],
            "opposite_faction" => &[
                "axis",
                "race",
                "at",
                "character",
                "unexpected_packets",
                "unexpected_subjects",
            ],
            _ => unreachable!(),
        };
        ensure!(
            fields.keys().all(|field| allowed.contains(&field.as_str())),
            "unsupported field in {axis} variant"
        );
        for field in ["watcher_movement", "values_only_assertions"] {
            if let Some(value) = declaration.get(field) {
                ensure!(value.is_bool(), "{field} must be a Boolean");
            }
        }
        for field in [
            "viewer_broadcasts",
            "scope_packets",
            "clocks",
            "identities",
            "tolerances",
            "packet_timing",
            "data_fields",
            "unexpected_packets",
        ] {
            if let Some(value) = declaration.get(field) {
                ensure!(value.is_array(), "{field} must be an array");
            }
        }
        if let Some(value) = declaration.get("character") {
            ensure!(value.is_str(), "variant Character must be a string");
        }
        if let Some(value) = declaration.get("replies") {
            ensure!(
                value
                    .as_array()
                    .is_some_and(|items| items.iter().all(Value::is_integer)),
                "reconnect replies must be an integer array"
            );
        }
        for field in ["exclude_self_for"] {
            if let Some(value) = declaration.get(field) {
                ensure!(
                    value
                        .as_array()
                        .is_some_and(|items| items.iter().all(Value::is_str)),
                    "{field} must be a string array"
                );
            }
        }
        let id = format!("{}__{axis}", base.id);
        ensure!(
            !derived.iter().any(|(existing, _)| existing == &id),
            "duplicate variant axis"
        );
        let mut value: Value = base_text.parse().context("base scenario TOML")?;
        value["id"] = Value::String(id.clone());
        if axis == "opposite_faction" {
            let race = declaration
                .get("race")
                .and_then(Value::as_str)
                .context("opposite faction needs a race")?;
            let character = declaration.get("character").and_then(Value::as_str);
            let target = if base.schema_version == 3 {
                ensure!(character.is_none(), "schema 3 has one Character");
                &mut value["fixtures"][0]
            } else {
                let character = character.context("schema 4 opposite faction needs a Character")?;
                let index = base
                    .characters
                    .iter()
                    .position(|entry| entry.id == character)
                    .context("opposite faction names an unknown Character")?;
                &mut value["characters"][index]["fixtures"][0]
            };
            let original = target["race"].as_str().context("base Character race")?;
            let alliance = |race: &str| -> Result<bool> {
                Ok(matches!(
                    crate::cli::parse_race(race)?.as_int(),
                    1 | 3 | 4 | 7
                ))
            };
            ensure!(
                alliance(race)? != alliance(original)?,
                "variant race must use the opposite faction"
            );
            target["race"] = Value::String(race.into());
            if let Some(at) = declaration.get("at") {
                target["at"] = at.clone();
            }
        } else {
            ensure!(base.schema_version == 3, "axis needs a schema 3 base");
            let fixtures = value
                .as_table_mut()
                .unwrap()
                .remove("fixtures")
                .context("base fixtures")?;
            let mut actor = toml::map::Map::new();
            actor.insert("id".into(), Value::String("actor".into()));
            actor.insert("fixtures".into(), fixtures);
            let mut characters = vec![Value::Table(actor)];
            if axis != "reconnect" {
                let viewer = declaration
                    .get("viewer")
                    .context("observer variant needs a viewer")?
                    .clone();
                ensure!(
                    viewer.get("verb").and_then(Value::as_str) == Some("character"),
                    "viewer must be a Character fixture"
                );
                let mut watcher = toml::map::Map::new();
                watcher.insert("id".into(), Value::String("viewer".into()));
                watcher.insert("fixtures".into(), Value::Array(vec![viewer]));
                if axis == "late_observer" {
                    watcher.insert("initially_connected".into(), Value::Boolean(false));
                }
                characters.push(Value::Table(watcher));
            }
            if axis == "spawn_beside_viewer" {
                characters.reverse();
                let creates = base
                    .fixtures
                    .iter()
                    .filter_map(|fixture| match fixture {
                        crate::scenario::FixtureVerb::Creature { id, .. } => {
                            let mut create = toml::map::Map::new();
                            create.insert("recipient".into(), Value::String("viewer".into()));
                            create.insert("actor".into(), Value::String("actor".into()));
                            create.insert("fixture".into(), Value::String(id.clone()));
                            Some(Value::Table(create))
                        }
                        _ => None,
                    })
                    .collect();
                value["opening"]
                    .as_table_mut()
                    .context("base opening")?
                    .insert("fixture_creates".into(), Value::Array(creates));
            }
            value
                .as_table_mut()
                .unwrap()
                .insert("characters".into(), Value::Array(characters));
            value["schema_version"] = Value::Integer(4);
            if let Some(scope) = value["rules"].get_mut("character_scope") {
                if axis != "reconnect" {
                    scope
                        .as_table_mut()
                        .unwrap()
                        .insert("all_characters".into(), Value::Boolean(true));
                }
                if let Some(extra) = declaration.get("scope_packets").and_then(Value::as_array) {
                    scope["packets"]
                        .as_array_mut()
                        .context("base scope packets")?
                        .extend(extra.iter().cloned());
                }
                for field in ["exclude_self_for", "exclude_create_values"] {
                    if let Some(value) = declaration.get(field) {
                        scope
                            .as_table_mut()
                            .unwrap()
                            .insert(field.into(), value.clone());
                    }
                }
            } else {
                ensure!(
                    declaration.get("scope_packets").is_none()
                        && declaration.get("exclude_self_for").is_none()
                        && declaration.get("exclude_create_values").is_none(),
                    "scope additions need a base Character scope"
                );
            }
            let steps = value["steps"].as_array_mut().context("base steps")?;
            for step in steps {
                let is_event_step =
                    step["id"].as_str() == declaration.get("step").and_then(Value::as_str);
                step.as_table_mut()
                    .unwrap()
                    .insert("character".into(), Value::String("actor".into()));
                let checkpoint = step["checkpoint"].as_table_mut().unwrap();
                let mut actor_capture = checkpoint.clone();
                actor_capture.remove("window_ms");
                actor_capture.insert("character".into(), Value::String("actor".into()));
                if declaration
                    .get("values_only_assertions")
                    .and_then(Value::as_bool)
                    == Some(true)
                {
                    if let Some(assertions) = actor_capture
                        .get_mut("expect")
                        .and_then(Value::as_array_mut)
                    {
                        for assertion in assertions {
                            if assertion.get("opcode").and_then(Value::as_integer) == Some(169) {
                                assertion
                                    .as_table_mut()
                                    .unwrap()
                                    .insert("values_only".into(), Value::Boolean(true));
                            }
                        }
                    }
                }
                let mut captures = vec![Value::Table(actor_capture)];
                if axis != "reconnect" {
                    let mut watcher_capture = toml::map::Map::new();
                    watcher_capture.insert("character".into(), Value::String("viewer".into()));
                    let mut broadcasts = checkpoint
                        .get("broadcasts")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    broadcasts
                        .retain(|item| !matches!(item["opcode"].as_integer(), Some(169 | 221)));
                    let mut item = toml::map::Map::new();
                    item.insert("opcode".into(), Value::Integer(169));
                    item.insert("stream".into(), Value::String("objects".into()));
                    broadcasts.push(Value::Table(item));
                    if is_event_step
                        && declaration.get("watcher_movement").and_then(Value::as_bool)
                            == Some(true)
                    {
                        let mut item = toml::map::Map::new();
                        item.insert("opcode".into(), Value::Integer(221));
                        item.insert("stream".into(), Value::String("movement".into()));
                        broadcasts.push(Value::Table(item));
                    }
                    if is_event_step {
                        if let Some(extra) = declaration
                            .get("viewer_broadcasts")
                            .and_then(Value::as_array)
                        {
                            broadcasts.extend(extra.iter().cloned());
                        }
                    }
                    watcher_capture.insert("broadcasts".into(), Value::Array(broadcasts));
                    if axis == "late_observer" && is_event_step {
                        watcher_capture.insert(
                            "creates".into(),
                            Value::Array(vec![Value::String("actor".into())]),
                        );
                    }
                    captures.push(Value::Table(watcher_capture));
                }
                checkpoint.remove("replies");
                checkpoint.remove("broadcasts");
                checkpoint.remove("expect");
                checkpoint.insert("characters".into(), Value::Array(captures));
            }
            if matches!(axis, "late_observer" | "reconnect") {
                let step_id = declaration
                    .get("step")
                    .and_then(Value::as_str)
                    .context("timed variant needs a step")?;
                let after_ms = declaration
                    .get("after_ms")
                    .and_then(Value::as_integer)
                    .context("timed variant needs after_ms")?;
                let step = value["steps"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|step| step["id"].as_str() == Some(step_id))
                    .context("variant names an unknown step")?;
                if let Some(replies) = declaration.get("replies") {
                    let original = base
                        .steps
                        .iter()
                        .find(|step| step.id == step_id)
                        .context("base step")?
                        .checkpoint
                        .replies
                        .iter();
                    let mut remaining = original.peekable();
                    for reply in replies.as_array().unwrap() {
                        if remaining
                            .peek()
                            .is_some_and(|opcode| reply.as_integer() == Some(i64::from(**opcode)))
                        {
                            remaining.next();
                        }
                    }
                    ensure!(
                        remaining.next().is_none(),
                        "reconnect replies must preserve base replies in order"
                    );
                    step["checkpoint"]["characters"][0]["replies"] = replies.clone();
                }
                let mut event = toml::map::Map::new();
                event.insert("after_ms".into(), Value::Integer(after_ms));
                event.insert(
                    "character".into(),
                    Value::String(
                        if axis == "late_observer" {
                            "viewer"
                        } else {
                            "actor"
                        }
                        .into(),
                    ),
                );
                let mut action = toml::map::Map::new();
                action.insert(
                    "kind".into(),
                    Value::String(
                        if axis == "late_observer" {
                            "connect"
                        } else {
                            "reconnect"
                        }
                        .into(),
                    ),
                );
                event.insert("action".into(), Value::Table(action));
                step.as_table_mut()
                    .unwrap()
                    .insert("events".into(), Value::Array(vec![Value::Table(event)]));
            }
            if axis == "spawn_beside_viewer"
                && !base
                    .fixtures
                    .iter()
                    .any(|f| matches!(f, crate::scenario::FixtureVerb::Creature { .. }))
            {
                bail!("spawn beside viewer needs a creature fixture");
            }
        }
        for field in [
            "clocks",
            "identities",
            "tolerances",
            "packet_timing",
            "data_fields",
            "unexpected_packets",
        ] {
            if let Some(extra) = declaration.get(field) {
                value["rules"]
                    .as_table_mut()
                    .unwrap()
                    .entry(field)
                    .or_insert_with(|| Value::Array(Vec::new()))
                    .as_array_mut()
                    .context("base rule array")?
                    .extend(
                        extra
                            .as_array()
                            .context("variant rules must be arrays")?
                            .iter()
                            .cloned(),
                    );
            }
        }
        if let Some(narrowed) = declaration.get("unexpected_subjects") {
            for selector in narrowed
                .as_array()
                .context("unexpected_subjects must be an array")?
            {
                ensure!(
                    selector.as_table().is_some_and(|fields| fields.len() == 2),
                    "unexpected subject selector needs name and subject_guid_high"
                );
                let name = selector
                    .get("name")
                    .and_then(Value::as_str)
                    .context("unexpected subject rule name")?;
                let high = selector
                    .get("subject_guid_high")
                    .and_then(Value::as_integer)
                    .context("unexpected subject high bits")?;
                ensure!(u16::try_from(high).is_ok(), "invalid subject high bits");
                let rule = value["rules"]["unexpected_packets"]
                    .as_array_mut()
                    .context("unexpected rules")?
                    .iter_mut()
                    .find(|rule| rule.get("name").and_then(Value::as_str) == Some(name))
                    .context("unknown unexpected rule to narrow")?;
                ensure!(
                    rule.get("subject_guid_high").is_none(),
                    "unexpected rule already has a subject selector"
                );
                rule.as_table_mut()
                    .unwrap()
                    .insert("subject_guid_high".into(), Value::Integer(high));
            }
        }
        let text = format!(
            "# Generated from {} and its variant declaration.\n# Base SHA-256: {}\n{}",
            base.id,
            String::from(Sha256Digest::of(base_text.as_bytes())),
            toml::to_string_pretty(&value)?
        );
        Scenario::parse(&text).with_context(|| format!("generated variant {id}"))?;
        derived.push((id, text));
    }
    Ok(derived)
}
