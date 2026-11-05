use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};
use wire_client::cli::Invocation;
use wire_client::decode::{decode, Field};
use wire_client::rolls::{CheckRecording, Counts, RecordedCharacter};
use wire_client::runner::FixtureAdapter;
use wire_client::scenario::{FixtureVerb, Observation, Scenario, Stream};
use wire_client::trace::{Header, Provenance, Trace};

use super::vmangos_fixture_impl::{command, drain_one, Adapter};

const NAMES: [&str; 16] = [
    "Rollalpha",
    "Rollbeta",
    "Rollgamma",
    "Rolldelta",
    "Rollepsilon",
    "Rollzeta",
    "Rolleta",
    "Rolltheta",
    "Rolliota",
    "Rollkappa",
    "Rolambda",
    "Rollmu",
    "Rollnu",
    "Rollxi",
    "Rollomicron",
    "Rollpi",
];

fn update_view(
    view: &mut BTreeMap<u64, BTreeMap<String, Field>>,
    opcode: u16,
    bytes: &[u8],
) -> Result<()> {
    if !matches!(opcode, 0x00a9 | 0x01f6) {
        return Ok(());
    }
    let packet = decode(opcode, bytes)?;
    apply_view(view, &packet);
    Ok(())
}

fn apply_view(
    view: &mut BTreeMap<u64, BTreeMap<String, Field>>,
    packet: &wire_client::decode::Packet,
) {
    let Some(Field::List(objects)) = packet.fields.get("objects") else {
        return;
    };
    for object in objects {
        if let Field::Object(object) = object {
            if let (Some(Field::Guid(guid)), Some(Field::Object(values))) =
                (object.get("guid"), object.get("values"))
            {
                view.entry(*guid).or_default().extend(values.clone());
            }
        }
    }
}

fn settle(
    gm: &mut wire_client::WireClient,
    client: &mut wire_client::WireClient,
    view: &mut BTreeMap<u64, BTreeMap<String, Field>>,
) -> Result<()> {
    drain_one(gm, Duration::from_millis(300))?;
    let until = Instant::now() + Duration::from_millis(300);
    while Instant::now() < until {
        if let Some((opcode, bytes)) = client.recv_raw_ready()? {
            update_view(view, opcode, &bytes)?;
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    Ok(())
}

fn verify(
    view: &BTreeMap<u64, BTreeMap<String, Field>>,
    character: u64,
    target: u64,
    level: u32,
) -> Result<()> {
    let player = view.get(&character).context("Character view")?;
    let creature = view.get(&target).context("target view")?;
    let word = |fields: &BTreeMap<String, Field>, index: usize| -> Result<u64> {
        match fields.get(&format!("field_{index}")) {
            Some(Field::Unsigned(value)) => Ok(*value),
            None => Ok(0),
            _ => anyhow::bail!("update word has wrong type"),
        }
    };
    ensure!(
        word(player, 34)? == 60
            && word(player, 151)? == 100
            && word(creature, 34)? == u64::from(level),
        "roll level/agility context changed"
    );
    ensure!(
        word(player, 22)? > 0 && word(creature, 22)? > 0,
        "roll participant died"
    );
    ensure!(
        word(player, 126)? == 2000,
        "ordinary unarmed cadence changed"
    );
    ensure!(
        f32::from_bits(word(player, 1109)? as u32) == 5.0,
        "roll base crit is not 5 percent"
    );
    for index in 486..524 {
        ensure!(word(player, index)? == 0, "roll Character has equipment");
    }
    for skill in [95u64, 162] {
        let slot = (718..1102)
            .step_by(3)
            .find(|index| word(player, *index).is_ok_and(|value| value & 0xffff == skill))
            .context("roll skill missing")?;
        ensure!(
            word(player, slot + 1)? == (300 | (300 << 16)),
            "roll skill changed"
        );
    }
    Ok(())
}

pub fn record(
    invocation: &Invocation,
    text: &str,
    provenance: Provenance,
    output: &str,
) -> Result<()> {
    let scenario = Scenario::parse(text)?;
    let spec = scenario.rolls.as_ref().context("roll specification")?;
    let peers = std::env::var("WIRE_PEER_ACCOUNTS")?;
    let peers: Vec<_> = peers.split(',').collect();
    ensure!(peers.len() == 15, "roll check needs fifteen peer Accounts");
    let gm_password = invocation.peer_password(15)?.to_owned();
    let FixtureVerb::Character { at, .. } = &scenario.fixtures[0] else {
        anyhow::bail!("roll Character fixture")
    };
    let setup = Instant::now();
    let mut clients = Vec::new();
    let mut views = Vec::new();
    for (index, name) in NAMES.iter().enumerate() {
        let mut own = invocation.clone();
        if index > 0 {
            own.account = peers[index - 1].into();
            own.password = invocation.peer_password(index - 1)?.into();
        }
        own.peer_passwords = vec![gm_password.clone()];
        let mut at = at.clone();
        at.y += index as f32 * 5.0;
        let fixture = FixtureVerb::Character {
            name: (*name).into(),
            race: "human".into(),
            class: "warrior".into(),
            level: 60,
            at: at.clone(),
        };
        let mut adapter = Adapter(own, vec![], None);
        let mut prepared = adapter.prepare(&[fixture])?;
        let mut view = BTreeMap::new();
        for observation in &prepared.opening {
            apply_view(&mut view, &observation.packet);
        }
        let level = spec.target_levels[index / 8];
        let gm = adapter.2.as_mut().context("roll GM")?;
        gm.set_selection(prepared.client.self_guid)?;
        for message in [
            ".additem 25 -1",
            ".additem 38 -1",
            ".additem 39 -1",
            ".additem 40 -1",
            ".additem 2362 -1",
            ".setskill 162 300 300",
            ".setskill 95 300 300",
            ".modify agility 100",
            ".modify hp 1000000000",
        ] {
            command(gm, message.into())?;
        }
        settle(gm, &mut prepared.client, &mut view)?;
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
            .context("roll target CREATE")?;
        adapter.1.push(target);
        gm.set_selection(target)?;
        command(gm, format!(".npc set level {level}"))?;
        command(gm, ".modify hp 1000000000".into())?;
        settle(gm, &mut prepared.client, &mut view)?;
        verify(&view, prepared.client.self_guid, target, level)?;
        views.push(view);
        adapter.2 = None;
        clients.push((adapter, prepared, target, level));
        eprintln!("[rolls] prepared {}/16", index + 1);
    }
    let setup_ms = setup.elapsed().as_millis();
    let started = Instant::now();
    let mut counts = vec![Counts::default(); 16];
    let mut blocks = vec![vec![Counts::default(); 5]; 16];

    let mut witnesses = Vec::new();
    let mut starts = [false; 16];
    let mut stops = [false; 16];
    for (_, prepared, target, _) in &mut clients {
        prepared.client.send_raw(0x0141, &target.to_le_bytes())?;
    }
    while counts.iter().any(|counts| counts.total() < 250) {
        ensure!(
            started.elapsed() < Duration::from_secs(1200),
            "roll check timed out"
        );
        for (index, (_, prepared, target, level)) in clients.iter_mut().enumerate() {
            while let Some((opcode, bytes)) = prepared.client.recv_raw_ready()? {
                update_view(&mut views[index], opcode, &bytes)?;
                if opcode == 0x0143 {
                    let packet = decode(opcode, &bytes)?;
                    if packet.fields.get("attacker")
                        == Some(&Field::Guid(prepared.client.self_guid))
                    {
                        starts[index] = true;
                        if index == 0 {
                            witnesses.push(Observation {
                                character: None,
                                received_after_ms: Some(0),
                                checkpoint: "start".into(),
                                stream: Stream::Reply("start".into()),
                                packet,
                            });
                        }
                    }
                }
                if opcode == 0x0144 {
                    let packet = decode(opcode, &bytes)?;
                    if packet.fields.get("attacker")
                        == Some(&Field::Guid(prepared.client.self_guid))
                    {
                        stops[index] = true;
                        if index == 0 {
                            witnesses.push(Observation {
                                character: None,
                                received_after_ms: Some(0),
                                checkpoint: "stop".into(),
                                stream: Stream::Reply("stop".into()),
                                packet,
                            });
                        }
                    }
                }
                if opcode != 0x014a || counts[index].total() == 250 {
                    continue;
                }
                let packet = decode(opcode, &bytes)?;
                let mut one = Counts::default();
                if one.observe(&packet, prepared.client.self_guid, *target)? {
                    let block = counts[index].total() as usize / 50;
                    for category in 0..7 {
                        counts[index].0[category] += one.0[category];
                        blocks[index][block].0[category] += one.0[category];
                    }
                    if counts[index].total() == 250 {
                        verify(&views[index], prepared.client.self_guid, *target, *level)?;
                        prepared.client.send_raw(0x0142, &[])?;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    for (index, (_, prepared, _, _)) in clients.iter_mut().enumerate() {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && !stops[index] {
            if let Some((opcode, bytes)) = prepared.client.recv_raw_ready()? {
                if opcode == 0x0144 {
                    let packet = decode(opcode, &bytes)?;
                    if packet.fields.get("attacker")
                        == Some(&Field::Guid(prepared.client.self_guid))
                    {
                        stops[index] = true;
                        if index == 0 {
                            witnesses.push(Observation {
                                character: None,
                                received_after_ms: Some(0),
                                checkpoint: "stop".into(),
                                stream: Stream::Reply("stop".into()),
                                packet,
                            });
                        }
                    }
                }
            } else {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    ensure!(
        starts.iter().all(|v| *v) && stops.iter().all(|v| *v),
        "missing attack start/stop witnesses"
    );
    let recording = CheckRecording {
        classifier_version: 1,
        elapsed_ms: started.elapsed().as_millis().try_into()?,
        characters: clients
            .iter()
            .enumerate()
            .map(|(index, (_, _, _, level))| RecordedCharacter {
                level: *level,
                counts: counts[index].clone(),
                blocks: blocks[index].clone(),
            })
            .collect(),
    };
    recording.validate(spec)?;
    let mut header = Header::golden(text, provenance, 0)?;
    header.character_guid = clients[0].1.client.self_guid;
    let mut trace = Trace::record(header, &witnesses)?;
    trace.roll_check = Some(recording);
    wire_client::trace::GoldenTrace::new(text, trace.clone())?;
    for (mut adapter, prepared, _, _) in clients {
        drop(prepared);
        adapter.cleanup()?;
    }
    trace.write_new(std::path::Path::new(output))?;
    eprintln!(
        "[rolls] setup {setup_ms} ms; recording {} ms",
        trace.roll_check.as_ref().unwrap().elapsed_ms
    );
    Ok(())
}
