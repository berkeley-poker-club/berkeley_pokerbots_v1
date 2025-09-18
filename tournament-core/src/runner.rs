use std::collections::{HashMap, HashSet, VecDeque};
use rand::{rngs::StdRng, SeedableRng, seq::SliceRandom};

use crate::config::TournamentConfig;
use crate::table_manager::{Table, TableFactory};
use table_runner::{TableId, TableHandle, TableEvent, TableCommand};
use crate::player_manager::PlayerId;
use poker_utils::LevelSpec;
use table_runner::GameTable;

pub type Placements = HashMap<PlayerId, usize>;
pub type SeriesResult = HashMap<PlayerId, Vec<usize>>;

use ordered_float::OrderedFloat;

#[derive(Debug)]
pub struct TournamentState {
    pub players: Vec<PlayerId>, 
    pub tables: Vec<Table>,
    pub waiting_lists: HashMap<TableId, VecDeque<PlayerId>>,
    pub level_index: usize,
    pub level_started_at: std::time::Instant,
    pub hands_played: HashMap<PlayerId, usize>,
    pub eliminated: Vec<(PlayerId, usize)>, // (player, hands_played)
    pub active_players: HashSet<PlayerId>,
    pub current_level: LevelSpec,
}

pub struct TournamentDirector {
    cfg: TournamentConfig,
    rng: StdRng,
    table_factory: TableFactory,
}

impl TournamentDirector {
    pub fn new(cfg: TournamentConfig) -> Self {
        Self {
            rng: StdRng::seed_from_u64(cfg.rng_seed),
            cfg,
            table_factory: TableFactory::new(),
        }
    }

    pub fn create_tables(&mut self, num_tables: usize) -> Vec<GameTable> {
        let mut tables = Vec::new();
        for _ in 0..num_tables {
            let rules = self.cfg.to_game_rules(0);
            let table = self.table_factory.create_table(self.cfg.table_size, rules);
            tables.push(table);
        }
        tables
    }

    pub async fn run_single_tournament_with_players(&mut self, players: Vec<PlayerId>) -> Placements {
        let num_tables = (players.len() + self.cfg.table_size - 1) / self.cfg.table_size;
        let tables = self.create_tables(num_tables);
        self.run_single_tournament(players, tables, self.cfg.initial_stack).await
    }

    pub async fn run_single_tournament<H: TableHandle>(
        &mut self,
        players: Vec<PlayerId>,
        mut tables: Vec<H>,
        total_chips_per_player: i64,
    ) -> Placements {
        let mut seat_lists = build_initial_tables(&players, self.cfg.table_size, &mut self.rng);

        rebalance_start(&mut seat_lists, self.cfg.min_table_size);

        for (i, t) in tables.iter().enumerate() {
            if let Some(list) = seat_lists.get(i) {
                for p in list {
                    let _ = t.send(TableCommand::SeatPlayer { player_id: p.clone() }).await;
                }
            }
            let lvl = &self.cfg.blind_levels[0];
            let _ = t.send(TableCommand::ApplyBlinds {
                level_id: lvl.level_id, small_blind: lvl.small_blind, big_blind: lvl.big_blind, ante: lvl.ante
            }).await;
        }
        for t in &tables { let _ = t.send(TableCommand::Resume).await; }

        let mut st = TournamentState {
            players: players.clone(),
            tables: tables.iter().map(|t| Table {
                id: t.id(), capacity: t.capacity(), active_count: 0, status_running: true
            }).collect(),
            waiting_lists: HashMap::new(),
            level_index: 0,
            level_started_at: std::time::Instant::now(),
            hands_played: players.iter().map(|p| (p.clone(), 0)).collect(),
            eliminated: vec![],
            active_players: players.iter().cloned().collect::<HashSet<_>>(),
            current_level: self.cfg.blind_levels[0].clone(),
        };

        // main loop
        while st.active_players.len() > 1 {
            for (i, t) in tables.iter().enumerate() {
                for ev in t.drain_events().await {
                    match ev {
                        TableEvent::HandEnded { participants, .. } => {
                            for p in participants {
                                if let Some(h) = st.hands_played.get_mut(&p) { *h += 1; }
                            }
                        }
                        TableEvent::PlayerBusted { player, .. } => {
                            st.active_players.remove(&player);
                            let hands_played = st.hands_played.get(&player).copied().unwrap_or(0);
                            st.eliminated.push((player, hands_played));
                        }
                        TableEvent::TableSizes { table_id, active_count } => {
                            if let Some(m) = st.tables.iter_mut().find(|m| m.id == table_id) {
                                m.active_count = active_count;
                            }
                        }
                        TableEvent::ReadyForReseat { .. } |
                        TableEvent::LevelApplied { .. } => {  }
                    }
                }

                // table breaking
                let meta = &st.tables[i];
                if meta.active_count > 0 && meta.active_count <= self.cfg.break_threshold {
                    let _ = t.send(TableCommand::CloseAfterHand).await;
                }
            }

            // blind advancement
            let total_chips = (st.active_players.len() as i64) * total_chips_per_player;
            if should_advance_level(&st, &self.cfg, total_chips) {
                for t in &tables { let _ = t.send(TableCommand::PauseAfterHand).await; }
                st.level_index += 1;
                if let Some(next) = self.cfg.blind_levels.get(st.level_index).cloned() {
                    for t in &tables {
                        let _ = t.send(TableCommand::ApplyBlinds {
                            level_id: next.level_id, small_blind: next.small_blind, big_blind: next.big_blind, ante: next.ante
                        }).await;
                    }
                    for t in &tables { let _ = t.send(TableCommand::Resume).await; }
                    st.level_started_at = std::time::Instant::now();
                    st.current_level = next;
                }
            }

        }

        // determine final placements
        let last = st.active_players.iter().next().cloned();
        finalize_placements(st.eliminated, last)
    }
}

pub fn build_initial_tables(players: &[PlayerId], capacity: usize, rng: &mut StdRng) -> Vec<Vec<PlayerId>> {
    let mut bag = players.to_vec();
    bag.shuffle(rng);
    bag.chunks(capacity).map(|c| c.to_vec()).collect()
}

pub fn rebalance_start(tables: &mut [Vec<PlayerId>], min_table_size: usize) {
    loop {
        let (min_i, max_i) = minmax_tables(tables);
        let ok_diff = tables[max_i].len().saturating_sub(tables[min_i].len()) <= 1;
        let ok_floor = tables.iter().all(|t| t.len() >= min_table_size);
        if ok_diff && ok_floor { break; }
        if let Some(p) = tables[max_i].pop() { tables[min_i].push(p); } else { break; }
    }
}

fn minmax_tables(tables: &[Vec<PlayerId>]) -> (usize, usize) {
    let mut min_i = 0;
    let mut max_i = 0;
    for i in 1..tables.len() {
        if tables[i].len() < tables[min_i].len() { min_i = i; }
        if tables[i].len() > tables[max_i].len() { max_i = i; }
    }
    (min_i, max_i)
}


pub fn should_advance_level(st: &TournamentState, cfg: &TournamentConfig, total_chips: i64) -> bool {
    let avg_stack = if st.active_players.is_empty() { 0 } else {
        total_chips / (st.active_players.len() as i64)
    };
    let time_elapsed = st.level_started_at.elapsed().as_secs();

    let stack_trigger = cfg.avg_stack_thresholds.iter().any(|(next_id, min_avg)| {
        *next_id as usize == st.level_index + 1 && avg_stack <= *min_avg
    });
    let time_trigger = time_elapsed >= cfg.max_level_duration_secs;
    stack_trigger || time_trigger
}

pub fn finalize_placements(mut eliminated: Vec<(PlayerId, usize)>, last_active: Option<PlayerId>) -> Placements {
    if let Some(winner) = last_active {
        eliminated.push((winner, hands_of(&eliminated, &winner)));
    }
    eliminated.sort_by(|a,b| b.1.cmp(&a.1));

    let mut placements = HashMap::new();
    let mut place = 1;
    let mut i = 0;
    while i < eliminated.len() {
        let j = (i+1..=eliminated.len()).find(|&k| k==eliminated.len() || eliminated[k-1].1 != eliminated[i].1).unwrap_or(eliminated.len());
        for k in i..j { placements.insert(eliminated[k].0.clone(), place); }
        place += j - i;
        i = j;
    }
    placements
}

fn hands_of(list: &[(PlayerId, usize)], id: &PlayerId) -> usize {
    list.iter().find(|(p,_)| p == id).map(|(_,h)| *h).unwrap_or(0)
}

pub fn geometric_mean_ranking(series: &HashMap<PlayerId, Vec<usize>>) -> Vec<(PlayerId, f64)> {
    let mut v: Vec<_> = series.iter().map(|(p, places)| {
        let g = if places.is_empty() { f64::INFINITY } else {
            let s: f64 = places.iter().map(|x| (*x as f64).ln()).sum();
            (s / places.len() as f64).exp()
        };
        (p.clone(), g)
    }).collect();
    v.sort_by_key(|(_, g)| OrderedFloat(*g));
    v
}