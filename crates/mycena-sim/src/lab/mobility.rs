// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! 2D mobility models for human movement patterns.

use crate::lab::rng::SimRng;

#[derive(Debug, Clone, Copy)]
pub struct Vec2 {
    pub x: f64,
    pub y: f64,
}

impl Vec2 {
    pub fn distance(&self, other: &Vec2) -> f64 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        (dx * dx + dy * dy).sqrt()
    }
}

#[derive(Debug, Clone)]
pub enum MobilityModel {
    /// Classic random waypoint: pick target, walk, pause, repeat.
    RandomWaypoint {
        speed_m_per_s: f64,
        pause_secs: f64,
        target: Option<Vec2>,
        pause_until_ms: u64,
    },
    /// Lévy flight: mostly short hops, occasional long jumps (urban pedestrian).
    LevyFlight {
        speed_m_per_s: f64,
        short_step_m: f64,
        long_step_m: f64,
        long_jump_prob: f64,
        target: Option<Vec2>,
    },
    /// Static cluster with Gaussian jitter (stadium / camp).
    StaticCluster { center: Vec2, jitter_m: f64 },
}

impl MobilityModel {
    pub fn random_waypoint(speed_m_per_s: f64) -> Self {
        Self::RandomWaypoint {
            speed_m_per_s,
            pause_secs: 2.0,
            target: None,
            pause_until_ms: 0,
        }
    }

    pub fn levy_flight_city() -> Self {
        Self::LevyFlight {
            speed_m_per_s: 1.4,
            short_step_m: 25.0,
            long_step_m: 400.0,
            long_jump_prob: 0.08,
            target: None,
        }
    }

    pub fn static_cluster(center: Vec2, jitter_m: f64) -> Self {
        Self::StaticCluster { center, jitter_m }
    }

    pub fn initial_position(
        &mut self,
        rng: &mut SimRng,
        node_idx: usize,
        world_size_m: f64,
    ) -> Vec2 {
        match self {
            Self::StaticCluster { center, jitter_m } => {
                let angle = rng.gen_range_f64(0.0, std::f64::consts::TAU);
                let r = rng.gen_range_f64(0.0, *jitter_m);
                Vec2 {
                    x: center.x + r * angle.cos(),
                    y: center.y + r * angle.sin(),
                }
            }
            _ => {
                let grid = (node_idx as f64 + 1.0).sqrt().ceil() as usize;
                let cell = world_size_m / grid.max(1) as f64;
                let gx = node_idx % grid;
                let gy = node_idx / grid;
                Vec2 {
                    x: rng.gen_range_f64(gx as f64 * cell, (gx + 1) as f64 * cell),
                    y: rng.gen_range_f64(gy as f64 * cell, (gy + 1) as f64 * cell),
                }
            }
        }
    }

    pub fn step(
        &mut self,
        rng: &mut SimRng,
        pos: &mut Vec2,
        now_ms: u64,
        dt_secs: f64,
        world_size_m: f64,
    ) {
        match self {
            Self::RandomWaypoint {
                speed_m_per_s,
                pause_secs,
                target,
                pause_until_ms,
            } => {
                if now_ms < *pause_until_ms {
                    return;
                }
                if target.is_none() {
                    *target = Some(Vec2 {
                        x: rng.gen_range_f64(0.0, world_size_m),
                        y: rng.gen_range_f64(0.0, world_size_m),
                    });
                }
                let Some(goal) = *target else {
                    return;
                };
                let dx = goal.x - pos.x;
                let dy = goal.y - pos.y;
                let dist = (dx * dx + dy * dy).sqrt();
                let step = *speed_m_per_s * dt_secs;
                if dist <= step || dist < 0.01 {
                    *target = None;
                    *pause_until_ms = now_ms + (*pause_secs * 1000.0) as u64;
                    return;
                }
                pos.x += dx / dist * step;
                pos.y += dy / dist * step;
            }
            Self::LevyFlight {
                speed_m_per_s,
                short_step_m,
                long_step_m,
                long_jump_prob,
                target,
            } => {
                if target.is_none() {
                    let step_len = if rng.gen_bool(*long_jump_prob) {
                        *long_step_m
                    } else {
                        rng.gen_range_f64(5.0, *short_step_m)
                    };
                    let angle = rng.gen_range_f64(0.0, std::f64::consts::TAU);
                    *target = Some(Vec2 {
                        x: pos.x + step_len * angle.cos(),
                        y: pos.y + step_len * angle.sin(),
                    });
                }
                let Some(goal) = *target else {
                    return;
                };
                let dx = goal.x - pos.x;
                let dy = goal.y - pos.y;
                let dist = (dx * dx + dy * dy).sqrt();
                let step = *speed_m_per_s * dt_secs;
                if dist <= step || dist < 0.01 {
                    *target = None;
                    return;
                }
                pos.x += dx / dist * step;
                pos.y += dy / dist * step;
            }
            Self::StaticCluster { center, jitter_m } => {
                let wobble = 0.05 * dt_secs;
                pos.x += rng.gen_range_f64(-wobble, wobble);
                pos.y += rng.gen_range_f64(-wobble, wobble);
                let d = pos.distance(center);
                if d > *jitter_m {
                    let scale = *jitter_m / d;
                    pos.x = center.x + (pos.x - center.x) * scale;
                    pos.y = center.y + (pos.y - center.y) * scale;
                }
            }
        }
    }

    pub fn clamp_to_world(&self, pos: &mut Vec2, world_size_m: f64) {
        pos.x = pos.x.clamp(0.0, world_size_m);
        pos.y = pos.y.clamp(0.0, world_size_m);
    }
}
