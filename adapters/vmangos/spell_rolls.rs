use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};
use wire_client::cli::Invocation;
use wire_client::decode::{decode, Field};
use wire_client::runner::FixtureAdapter;
use wire_client::scenario::{FixtureVerb, Observation, Scenario, Stream};
use wire_client::spell_rolls::{CheckRecording, Collector, Counts, RecordedCharacter};
use wire_client::trace::{Header, Provenance, Trace};
use wire_client::WireClient;

use super::vmangos_fixture_impl::{command, drain_one, Adapter};
use super::vmangos_rolls::{
    apply_view, settle, update_view, verify_max_health, SpawnedTargets, NAMES,
};

fn verify(
    view: &BTreeMap<u64, BTreeMap<String, Field>>,
    caster: u64,
    target: u64,
    level: u32,
) -> Result<()> {
    let player = view.get(&caster).context("spell Character view")?;
    let creature = view.get(&target).context("spell target view")?;
    let word = |fields: &BTreeMap<String, Field>, index| -> Result<u64> {
        match fields.get(&format!("field_{index}")) {
            Some(Field::Unsigned(value)) => Ok(*value),
            None => Ok(0),
            _ => anyhow::bail!("invalid spell context word"),
        }
    };
    ensure!(
        word(player, 34)? == 60
            && word(player, 36)? & 0xffff == 0x0801
            && word(player, 153)? == 125
            && word(creature, 34)? == u64::from(level)
            && word(creature, 35)? == 7,
        "spell level, race, class, intellect or faction changed"
    );
    ensure!(
        word(player, 22)? > 0
            && word(creature, 22)? > 0
            && word(player, 28)? == 1_000_000_000
            && word(creature, 28)? == 100
            && word(player, 23)? > 50
            && word(player, 29)? == 1_000_000_000,
        "spell health or mana context changed"
    );
    for fields in [player, creature] {
        for index in 47..95 {
            ensure!(word(fields, index)? == 0, "spell participant has an aura");
        }
    }
    for index in 486..524 {
        ensure!(word(player, index)? == 0, "spell Character has equipment");
    }
    for (fields, offset, orientation) in [(player, 0.0, 0.0), (creature, 2.0, std::f64::consts::PI)]
    {
        let Some(Field::Object(movement)) = fields.get("fixture_movement") else {
            anyhow::bail!("spell movement context absent")
        };
        let Some(Field::Object(position)) = movement.get("position") else {
            anyhow::bail!("spell position absent")
        };
        ensure!(
            matches!(movement.get("orientation"), Some(Field::Float(actual)) if (actual - orientation).abs() < 0.001),
            "spell participant turned"
        );
        for (axis, expected) in [("x", 16222.1 + offset), ("y", 16252.1), ("z", 12.5872)] {
            ensure!(
                matches!(position.get(axis), Some(Field::Float(actual)) if (actual - expected).abs() < 0.02),
                "spell participant moved on {axis}"
            );
        }
    }
    Ok(())
}

fn gm_confirmation(gm: &mut WireClient, message: &str, expected: &str) -> Result<()> {
    drain_one(gm, Duration::from_millis(100))?;
    command(gm, message.into())?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some((opcode, bytes)) = gm.recv_raw_ready()? {
            if opcode == 0x96
                && decode(opcode, &bytes)?
                    .fields
                    .values()
                    .any(|field| matches!(field, Field::Text(text) if text.contains(expected)))
            {
                return Ok(());
            }
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    anyhow::bail!("GM did not confirm spell fixture command {message}")
}

fn ready_cast(client: &mut WireClient, target: u64) -> Result<()> {
    let mut collector = Collector::new(client.self_guid, target, 2948, 2, 1500)?;
    collector.begin_cast()?;
    client.cast_spell(2948, target)?;
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        if let Some((opcode, bytes)) = client.recv_raw_ready()? {
            if matches!(opcode, 0x130 | 0x131 | 0x132 | 0x250)
                && collector.observe(&decode(opcode, &bytes)?)?
            {
                return Ok(());
            }
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    anyhow::bail!("fixture readiness cast incomplete")
}

pub fn record(
    invocation: &Invocation,
    text: &str,
    provenance: Provenance,
    output: &str,
) -> Result<()> {
    let scenario = Scenario::parse(text)?;
    let spec = scenario
        .spell_rolls
        .as_ref()
        .context("spell roll specification")?;
    ensure!(
        spec.spell_id == 2948
            && spec.intellect == 125
            && spec.target_entry == 38
            && spec.target_faction == 7
            && spec.target_levels == [60, 63],
        "unsupported Reference Server spell context"
    );
    let peers = std::env::var("WIRE_PEER_ACCOUNTS")?;
    let peers: Vec<_> = peers.split(',').collect();
    ensure!(peers.len() == 15, "spell check needs fifteen peer Accounts");
    let gm_password = invocation.peer_password(15)?.to_owned();
    let FixtureVerb::Character {
        name,
        race,
        class,
        level,
        at,
    } = &scenario.fixtures[0]
    else {
        anyhow::bail!("spell Character fixture")
    };
    ensure!(
        name == NAMES[0]
            && race == "human"
            && class == "mage"
            && *level == 60
            && at.map == 1
            && (at.x - 16222.1).abs() < 0.01
            && (at.y - 16252.1).abs() < 0.01
            && (at.z - 12.5872).abs() < 0.01
            && at.orientation == 0.0
            && scenario.fixtures.len() == 3
            && matches!(&scenario.fixtures[1], FixtureVerb::Spell { spell_id: 2948 })
            && matches!(&scenario.fixtures[2], FixtureVerb::Creature { id, entry: 38, offset } if id == "target" && *offset == 2.0),
        "unsupported Reference Server spell fixtures"
    );
    let setup = Instant::now();
    let mut cleanup_invocation = invocation.clone();
    cleanup_invocation.peer_passwords = vec![gm_password.clone()];
    let mut targets = SpawnedTargets(Adapter(cleanup_invocation, vec![], None));
    let mut clients = Vec::new();
    let mut views = Vec::new();
    for (index, name) in NAMES.iter().enumerate() {
        let mut own = invocation.clone();
        if index > 0 {
            own.account = peers[index - 1].into();
            own.password = invocation.peer_password(index - 1)?.into();
        }
        own.peer_passwords = vec![gm_password.clone()];
        let fixture = FixtureVerb::Character {
            name: (*name).into(),
            race: "human".into(),
            class: "mage".into(),
            level: 60,
            at: at.clone(),
        };
        let mut adapter = Adapter(own, vec![], None);
        let mut prepared = adapter.prepare(&[
            fixture,
            FixtureVerb::Spell {
                spell_id: spec.spell_id,
            },
        ])?;
        let mut view = BTreeMap::new();
        for observation in &prepared.opening {
            apply_view(&mut view, &observation.packet);
        }
        let level = spec.target_levels[index / 8];
        let gm = adapter.2.as_mut().context("spell GM")?;
        gm.set_selection(prepared.client.self_guid)?;
        settle(gm, &mut prepared.client, &mut view)?;
        for message in [
            ".unaura all",
            ".additem 35 -1",
            ".additem 55 -1",
            ".additem 56 -1",
            ".additem 1395 -1",
            ".additem 6096 -1",
            ".modify intellect 0",
            ".modify hp 1000000000",
            ".modify mana 1000000000",
        ] {
            command(gm, message.into())?;
            settle(gm, &mut prepared.client, &mut view)?;
        }
        let mut target_at = at.clone();
        target_at.x += 2.0;
        target_at.orientation = std::f32::consts::PI;
        command(
            gm,
            format!(
                ".go xyzo {} {} {} {} {}",
                target_at.x, target_at.y, target_at.z, target_at.orientation, target_at.map
            ),
        )?;
        settle(gm, &mut prepared.client, &mut view)?;
        let before: std::collections::BTreeSet<_> = view.keys().copied().collect();
        command(gm, ".npc spawn add 38".into())?;
        settle(gm, &mut prepared.client, &mut view)?;
        let target = *view
            .iter()
            .find(|(guid, fields)| {
                !before.contains(guid) && fields.get("field_3") == Some(&Field::Unsigned(38))
            })
            .map(|(guid, _)| guid)
            .context("spell target CREATE")?;
        targets.0 .1.push(target);
        gm.set_selection(target)?;
        settle(gm, &mut prepared.client, &mut view)?;
        for message in [
            ".npc spawn set movetype idle".into(),
            ".npc spawn move".into(),
            ".npc set faction 7".into(),
            format!(".npc set level {level}"),
            ".unaura all".into(),
            ".modify hp 1000000000".into(),
        ] {
            command(gm, message)?;
            settle(gm, &mut prepared.client, &mut view)?;
        }
        gm_confirmation(gm, ".npc set reactstate 0", "updated to 0.")?;
        gm_confirmation(
            gm,
            ".debug getvaluebyindex 157 int",
            "[FIELD]:157 [VALUE]:0",
        )?;
        verify_max_health(gm, target)?;
        verify(&view, prepared.client.self_guid, target, level)?;
        ready_cast(&mut prepared.client, target)?;
        settle(gm, &mut prepared.client, &mut view)?;
        verify(&view, prepared.client.self_guid, target, level)?;
        views.push(view);
        adapter.2 = None;
        clients.push((adapter, prepared, target, level));
        eprintln!("[spell rolls] prepared {}/16", index + 1);
    }
    for (index, (_, prepared, target, level)) in clients.iter_mut().enumerate() {
        while let Some((opcode, bytes)) = prepared.client.recv_raw_ready()? {
            update_view(&mut views[index], opcode, &bytes)?;
        }
        verify(&views[index], prepared.client.self_guid, *target, *level)?;
    }
    let setup_ms = setup.elapsed().as_millis().try_into()?;
    let mut collectors: Vec<_> = clients
        .iter()
        .map(|(_, p, target, _)| {
            Collector::new(p.client.self_guid, *target, spec.spell_id, 2, 1500)
        })
        .collect::<Result<_>>()?;
    let mut blocks = vec![vec![Counts::default(); 5]; 16];
    let mut last_cast = vec![Instant::now(); 16];
    let mut next_cast = vec![Instant::now(); 16];
    let mut witnesses = Vec::new();
    let mut next_report = Duration::from_secs(30);
    let started = Instant::now();
    while collectors.iter().any(|c| c.counts.total() < 250) {
        ensure!(
            started.elapsed() < Duration::from_secs(1200),
            "spell check timed out"
        );
        for (index, (_, prepared, target, level)) in clients.iter_mut().enumerate() {
            while let Some((opcode, bytes)) = prepared.client.recv_raw_ready()? {
                update_view(&mut views[index], opcode, &bytes)?;
                if !matches!(opcode, 0x130 | 0x131 | 0x132 | 0x250) {
                    continue;
                }
                let packet = decode(opcode, &bytes)?;
                if opcode == 0x131
                    && index == 0
                    && witnesses.is_empty()
                    && packet.fields.get("caster") == Some(&Field::Guid(prepared.client.self_guid))
                {
                    ensure!(
                        packet.fields.get("spell") == Some(&Field::Unsigned(2948))
                            && packet.fields.get("duration") == Some(&Field::TimeMs(1500)),
                        "ordinary Scorch cast time changed"
                    );
                    witnesses.push(Observation {
                        character: None,
                        received_after_ms: Some(0),
                        checkpoint: "cast".into(),
                        stream: Stream::Reply("cast".into()),
                        packet: packet.clone(),
                    });
                }
                let previous = collectors[index].counts.clone();
                if collectors[index].observe(&packet)? {
                    let block = previous.total() as usize / 50;
                    for category in 0..5 {
                        blocks[index][block].0[category] +=
                            collectors[index].counts.0[category] - previous.0[category];
                    }
                    last_cast[index] = Instant::now();
                    verify(&views[index], prepared.client.self_guid, *target, *level)?;
                }
            }
            if collectors[index].counts.total() < 250 {
                ensure!(
                    last_cast[index].elapsed() < Duration::from_secs(8),
                    "Character {index} spell outcome missing"
                );
                if collectors[index].idle() && Instant::now() >= next_cast[index] {
                    collectors[index].begin_cast()?;
                    prepared.client.cast_spell(spec.spell_id, *target)?;
                    next_cast[index] = Instant::now() + Duration::from_millis(1600);
                }
            }
        }
        if started.elapsed() >= next_report {
            eprintln!(
                "[spell rolls] {:.1}s; Character totals {:?}",
                started.elapsed().as_secs_f64(),
                collectors
                    .iter()
                    .map(|c| c.counts.total())
                    .collect::<Vec<_>>()
            );
            next_report += Duration::from_secs(30);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    ensure!(witnesses.len() == 1, "spell cast witness missing");
    let recording = CheckRecording {
        classifier_version: 1,
        setup_ms,
        elapsed_ms: started.elapsed().as_millis().try_into()?,
        characters: clients
            .iter()
            .enumerate()
            .map(|(index, (_, _, _, level))| RecordedCharacter {
                level: *level,
                counts: collectors[index].counts.clone(),
                blocks: blocks[index].clone(),
            })
            .collect(),
    };
    eprintln!(
        "[spell rolls] completed diagnostic: {}",
        serde_json::to_string(&recording)?
    );
    recording.validate(spec)?;
    let mut gm_inv = invocation.clone();
    gm_inv.account = std::env::var("WIRE_GM_ACCOUNT")?;
    gm_inv.character = std::env::var("WIRE_GM_CHARACTER")?;
    gm_inv.password = gm_password;
    let mut gm = super::connect(&gm_inv)?;
    let guid = gm.create_or_find_char(
        &gm_inv.character,
        wow_world_messages::vanilla::Class::Warrior,
    )?;
    gm.player_login(guid)?;
    for (_, _, target, _) in &clients {
        verify_max_health(&mut gm, *target)?;
        gm_confirmation(
            &mut gm,
            ".debug getvaluebyindex 157 int",
            "[FIELD]:157 [VALUE]:0",
        )?;
    }
    drop(gm);
    let recorded_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis()
        .try_into()?;
    let mut header = Header::golden(text, provenance, recorded_at)?;
    header.character_guid = clients[0].1.client.self_guid;
    let mut trace = Trace::record(header, &witnesses)?;
    trace.spell_roll_check = Some(recording);
    wire_client::trace::GoldenTrace::new(text, trace.clone())?;
    drop(clients);
    targets.0.cleanup()?;
    trace.write_new(std::path::Path::new(output))?;
    eprintln!(
        "[spell rolls] setup_ms={setup_ms}; capture_ms={}",
        trace
            .spell_roll_check
            .as_ref()
            .context("spell check")?
            .elapsed_ms
    );
    Ok(())
}
