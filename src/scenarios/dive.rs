//! Breath flow: submerge, observe the breath timer and drowning, then surface.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Result};
use wire_client::WireClient;
use wow_world_base::shared::environmental_damage_type_vanilla_tbc_wrath::EnvironmentalDamageType;
use wow_world_base::shared::timer_type_vanilla_tbc_wrath::TimerType;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage as Smsg;
use wow_world_messages::vanilla::{
    LogoutResult, MSG_MOVE_HEARTBEAT_Client, MovementInfo, MovementInfo_MovementFlags,
    MovementInfo_MovementFlags_Swimming, Vector3d, SMSG_START_MIRROR_TIMER,
};

use super::ModeCtx;

const START_WINDOW: Duration = Duration::from_secs(5);
const STOP_WINDOW: Duration = Duration::from_secs(5);
const DAMAGE_GRACE: Duration = Duration::from_secs(8);
// Breath is advanced by a one-second server tick. Allow one tick plus delivery jitter, while still
// refusing damage that arrives before the advertised timer has substantially drained.
const DAMAGE_EARLY_TOLERANCE: Duration = Duration::from_millis(1_500);
const MAX_TIMER_MS: u32 = 120_000;
const USAGE: &str =
    "scenario-dive <submerged_x> <submerged_y> <submerged_z> <surface_x> <surface_y> <surface_z>";

pub(crate) fn try_dispatch(
    mode: &str,
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<bool> {
    if mode != "scenario-dive" {
        return Ok(false);
    }
    scenario_dive(c, args)?;
    Ok(true)
}

fn parse_coord(args: &mut dyn Iterator<Item = String>, name: &str) -> Result<f32> {
    let raw = args
        .next()
        .ok_or_else(|| anyhow!("usage: {USAGE}: missing <{name}>"))?;
    let value: f32 = raw
        .parse()
        .map_err(|_| anyhow!("usage: {USAGE}: invalid <{name}>: {raw:?}"))?;
    if !value.is_finite() {
        bail!("usage: {USAGE}: <{name}> must be finite");
    }
    Ok(value)
}

fn movement_timestamp() -> u32 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    (millis % u64::from(u32::MAX - MAX_TIMER_MS - 10_000)) as u32 + 1
}

fn heartbeat(position: [f32; 3], timestamp: u32, swimming: bool) -> MSG_MOVE_HEARTBEAT_Client {
    let flags = if swimming {
        MovementInfo_MovementFlags::new_swimming(MovementInfo_MovementFlags_Swimming { pitch: 0.0 })
    } else {
        MovementInfo_MovementFlags::empty()
    };
    MSG_MOVE_HEARTBEAT_Client {
        info: MovementInfo {
            flags,
            timestamp,
            position: Vector3d {
                x: position[0],
                y: position[1],
                z: position[2],
            },
            orientation: 0.0,
            fall_time: 0.0,
        },
    }
}

fn validate_breath_timer(timer: SMSG_START_MIRROR_TIMER) -> Result<u32> {
    if timer.timer != TimerType::Breath {
        bail!(
            "STEP 1 FAIL: mirror timer type was {}, want Breath",
            timer.timer
        );
    }
    if timer.time_remaining == 0
        || timer.time_remaining > timer.duration
        || timer.duration > MAX_TIMER_MS
    {
        bail!(
            "STEP 1 FAIL: invalid breath timer remaining={}ms duration={}ms",
            timer.time_remaining,
            timer.duration
        );
    }
    if timer.scale != u32::MAX || timer.is_frozen {
        bail!(
            "STEP 1 FAIL: breath timer scale={} frozen={}, want scale=-1 and frozen=false",
            timer.scale,
            timer.is_frozen
        );
    }
    Ok(timer.time_remaining)
}

#[derive(Default)]
struct DrowningProof {
    damage: u32,
    wrong_recipient: Option<u64>,
}

impl DrowningProof {
    fn observe(
        &mut self,
        damage_type: EnvironmentalDamageType,
        recipient: u64,
        damage: u32,
        own_guid: u64,
    ) {
        if damage_type != EnvironmentalDamageType::Drowning || damage == 0 {
            return;
        }
        if recipient == own_guid {
            self.damage = self.damage.saturating_add(damage);
        } else {
            self.wrong_recipient = Some(recipient);
        }
    }

    fn matches_health(&self, initial_health: u32, current_health: Option<u32>) -> bool {
        self.damage > 0
            && current_health.map(|health| initial_health.saturating_sub(health))
                == Some(self.damage)
    }
}

fn drowning_damage_is_due(elapsed: Duration, advertised_remaining_ms: u32) -> bool {
    elapsed + DAMAGE_EARLY_TOLERANCE >= Duration::from_millis(u64::from(advertised_remaining_ms))
}

fn scenario_dive(c: &mut WireClient, args: &mut dyn Iterator<Item = String>) -> Result<()> {
    let submerged = [
        parse_coord(args, "submerged_x")?,
        parse_coord(args, "submerged_y")?,
        parse_coord(args, "submerged_z")?,
    ];
    let surface = [
        parse_coord(args, "surface_x")?,
        parse_coord(args, "surface_y")?,
        parse_coord(args, "surface_z")?,
    ];
    if args.next().is_some() {
        bail!("usage: {USAGE}: unexpected extra argument");
    }

    let initial_health = c
        .own_character_health()
        .filter(|health| *health > 0)
        .ok_or_else(|| anyhow!("STEP 0 FAIL: login snapshot did not show positive own health"))?;
    let started_at = Instant::now();
    let submerged_timestamp = movement_timestamp();
    c.set_recv_timeout(Duration::from_millis(400))?;
    c.send(&heartbeat(submerged, submerged_timestamp, true))?;

    let mut wrong_timer = None;
    let start = c.recv_for(START_WINDOW, |message| match message {
        Smsg::SMSG_START_MIRROR_TIMER(timer) if timer.timer == TimerType::Breath => Some(**timer),
        Smsg::SMSG_START_MIRROR_TIMER(timer) => {
            wrong_timer = Some(timer.timer);
            None
        }
        _ => None,
    });
    let Some(start) = start else {
        bail!(
            "STEP 1 FAIL: no Breath SMSG_START_MIRROR_TIMER within 5s{}",
            wrong_timer.map_or_else(String::new, |timer| format!("; observed {timer}"))
        );
    };
    let remaining_ms = validate_breath_timer(start)?;
    let timer_received_at = Instant::now();
    println!("[scenario] STEP 1 OK: Breath timer started with {remaining_ms}ms remaining");

    let deadline =
        timer_received_at + Duration::from_millis(u64::from(remaining_ms)) + DAMAGE_GRACE;
    let mut proof = DrowningProof::default();
    while Instant::now() < deadline {
        match c.recv() {
            Ok(Smsg::SMSG_ENVIRONMENTAL_DAMAGE_LOG(log)) => {
                if log.damage_type == EnvironmentalDamageType::Drowning
                    && log.guid.guid() == c.self_guid
                    && log.damage > 0
                    && !drowning_damage_is_due(timer_received_at.elapsed(), remaining_ms)
                {
                    bail!(
                        "STEP 2 FAIL: own drowning damage arrived before the advertised Breath window drained"
                    );
                }
                proof.observe(log.damage_type, log.guid.guid(), log.damage, c.self_guid);
            }
            Ok(_) => {}
            Err(error) if wire_client::is_read_timeout(&error) => {}
            Err(error) => return Err(error),
        }

        if let Some(health) = c.own_character_health() {
            let observed_drop = initial_health.saturating_sub(health);
            if proof.matches_health(initial_health, Some(health)) {
                println!(
                    "[scenario] STEP 2 OK: own Drowning log matched health drop {observed_drop}"
                );
                break;
            }
        }
    }
    let final_health = c.own_character_health();
    if !proof.matches_health(initial_health, final_health) {
        bail!(
            "STEP 2 FAIL: no matched own drowning damage before timer budget ended (logged={}, health {initial_health}->{final_health:?}, wrong recipient={:?})",
            proof.damage,
            proof.wrong_recipient
        );
    }

    let elapsed_ms = started_at
        .elapsed()
        .as_millis()
        .min(u128::from(MAX_TIMER_MS)) as u32;
    let surface_timestamp = submerged_timestamp
        .saturating_add(elapsed_ms)
        .saturating_add(1);
    c.send(&heartbeat(surface, surface_timestamp, false))?;
    let mut wrong_stop = None;
    let stopped = c.recv_for(STOP_WINDOW, |message| match message {
        Smsg::SMSG_STOP_MIRROR_TIMER(stop) if stop.timer == TimerType::Breath => Some(()),
        Smsg::SMSG_STOP_MIRROR_TIMER(stop) => {
            wrong_stop = Some(stop.timer);
            None
        }
        _ => None,
    });
    if stopped.is_none() {
        bail!(
            "STEP 3 FAIL: no Breath SMSG_STOP_MIRROR_TIMER within 5s{}",
            wrong_stop.map_or_else(String::new, |timer| format!("; observed {timer}"))
        );
    }
    if c.own_character_is_dead() == Some(true) || c.own_character_health() == Some(0) {
        bail!("STEP 3 FAIL: Character died before the surface timer stop");
    }
    println!("[scenario] STEP 3 OK: surface movement stopped the Breath timer");

    c.set_recv_timeout(Duration::from_secs(10))?;
    if c.logout_request()? != LogoutResult::Success {
        bail!("STEP 4 FAIL: logout was refused");
    }
    println!("[wire] SCENARIO-DIVE PASS: timer start, matched drowning damage, timer stop");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_sets_swimming_only_underwater() {
        let submerged = heartbeat([1.0, 2.0, 3.0], 1000, true);
        assert!(submerged.info.flags.get_swimming().is_some());
        assert_eq!(submerged.info.timestamp, 1000);
        assert_eq!(submerged.info.position.z, 3.0);

        let surface = heartbeat([1.0, 2.0, 5.0], 2000, false);
        assert!(surface.info.flags.get_swimming().is_none());
        assert_eq!(surface.info.timestamp, 2000);
    }

    #[test]
    fn breath_timer_bounds_drive_the_wait_budget() {
        let valid = SMSG_START_MIRROR_TIMER {
            timer: TimerType::Breath,
            time_remaining: 60_000,
            duration: 60_000,
            scale: u32::MAX,
            is_frozen: false,
            id: 0,
        };
        assert_eq!(validate_breath_timer(valid).unwrap(), 60_000);

        for invalid in [
            SMSG_START_MIRROR_TIMER {
                timer: TimerType::Fatigue,
                ..valid
            },
            SMSG_START_MIRROR_TIMER {
                time_remaining: 0,
                ..valid
            },
            SMSG_START_MIRROR_TIMER {
                duration: MAX_TIMER_MS + 1,
                ..valid
            },
            SMSG_START_MIRROR_TIMER { scale: 1, ..valid },
            SMSG_START_MIRROR_TIMER {
                is_frozen: true,
                ..valid
            },
        ] {
            assert!(validate_breath_timer(invalid).is_err());
        }
    }

    #[test]
    fn coordinate_parser_rejects_non_finite_values() {
        for value in ["NaN", "inf", "-inf", "words"] {
            let mut args = [value.to_string()].into_iter();
            assert!(parse_coord(&mut args, "x").is_err());
        }
    }

    #[test]
    fn drowning_proof_requires_own_log_and_matching_health_loss() {
        let own_guid = 42;
        let mut proof = DrowningProof::default();

        proof.observe(EnvironmentalDamageType::Drowning, 99, 20, own_guid);
        proof.observe(EnvironmentalDamageType::Fall, own_guid, 20, own_guid);
        assert!(!proof.matches_health(100, Some(80)));

        proof.observe(EnvironmentalDamageType::Drowning, own_guid, 20, own_guid);
        assert!(!proof.matches_health(100, None));
        assert!(!proof.matches_health(100, Some(100)));
        assert!(!proof.matches_health(100, Some(79)));
        assert!(proof.matches_health(100, Some(80)));
    }

    #[test]
    fn drowning_damage_must_wait_through_the_advertised_window() {
        assert!(!drowning_damage_is_due(Duration::from_secs(30), 60_000));
        assert!(!drowning_damage_is_due(
            Duration::from_millis(58_499),
            60_000
        ));
        assert!(drowning_damage_is_due(
            Duration::from_millis(58_500),
            60_000
        ));
        assert!(drowning_damage_is_due(Duration::from_secs(60), 60_000));
    }
}
