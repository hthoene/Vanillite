//! Weather: the server's `WeatherData` (saved in `data/minecraft/weather.dat`) and the rain and
//! thunder levels of a world (`ServerLevel.advanceWeatherCycle`).

use vanillite_base::random::RandomSource;
use vanillite_nbt::{Compound, Tag};
use vanillite_protocol::packets::play::clientbound::GameEvent;

/// `ServerLevel.RAIN_DELAY`.
pub const RAIN_DELAY: (i32, i32) = (12_000, 180_000);
/// `ServerLevel.RAIN_DURATION`.
pub const RAIN_DURATION: (i32, i32) = (12_000, 24_000);
/// `ServerLevel.THUNDER_DELAY`.
pub const THUNDER_DELAY: (i32, i32) = (12_000, 180_000);
/// `ServerLevel.THUNDER_DURATION`.
pub const THUNDER_DURATION: (i32, i32) = (3_600, 15_600);

/// Game event ids of `ClientboundGameEventPacket`.
pub mod event {
    pub const NO_RESPAWN_BLOCK_AVAILABLE: u8 = 0;
    pub const START_RAINING: u8 = 1;
    pub const STOP_RAINING: u8 = 2;
    pub const RAIN_LEVEL_CHANGE: u8 = 7;
    pub const THUNDER_LEVEL_CHANGE: u8 = 8;
    pub const IMMEDIATE_RESPAWN: u8 = 11;
    pub const LIMITED_CRAFTING: u8 = 12;
}

/// `UniformInt.sample`.
pub fn sample(range: (i32, i32), random: &mut impl RandomSource) -> i32 {
    random.next_int_between_inclusive(range.0, range.1)
}

/// `WeatherData`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WeatherData {
    pub clear_weather_time: i32,
    pub rain_time: i32,
    pub thunder_time: i32,
    pub raining: bool,
    pub thundering: bool,
}

impl WeatherData {
    /// `MinecraftServer.setWeatherParameters` (`/weather`).
    pub fn set(&mut self, clear_time: i32, rain_time: i32, raining: bool, thundering: bool) {
        self.clear_weather_time = clear_time;
        self.rain_time = rain_time;
        self.thunder_time = rain_time;
        self.raining = raining;
        self.thundering = thundering;
    }

    /// `ServerLevel.resetWeatherCycle`.
    pub fn reset_cycle(&mut self) {
        self.rain_time = 0;
        self.raining = false;
        self.thunder_time = 0;
        self.thundering = false;
    }

    /// The timers of `advanceWeatherCycle` (with `advance_weather` on).
    pub fn advance(&mut self, random: &mut impl RandomSource) {
        if self.clear_weather_time > 0 {
            self.clear_weather_time -= 1;
            self.thunder_time = if self.thundering { 0 } else { 1 };
            self.rain_time = if self.raining { 0 } else { 1 };
            self.thundering = false;
            self.raining = false;
        } else {
            if self.thunder_time > 0 {
                self.thunder_time -= 1;
                if self.thunder_time == 0 {
                    self.thundering = !self.thundering;
                }
            } else if self.thundering {
                self.thunder_time = sample(THUNDER_DURATION, random);
            } else {
                self.thunder_time = sample(THUNDER_DELAY, random);
            }
            if self.rain_time > 0 {
                self.rain_time -= 1;
                if self.rain_time == 0 {
                    self.raining = !self.raining;
                }
            } else if self.raining {
                self.rain_time = sample(RAIN_DURATION, random);
            } else {
                self.rain_time = sample(RAIN_DELAY, random);
            }
        }
    }

    /// `WeatherData.CODEC` from the `data` compound of `weather.dat`.
    pub fn load(data: &Compound) -> WeatherData {
        let int = |k: &str| {
            data.get(k)
                .and_then(Tag::as_i64)
                .and_then(|v| i32::try_from(v).ok())
                .unwrap_or(0)
        };
        WeatherData {
            clear_weather_time: int("clear_weather_time"),
            rain_time: int("rain_time"),
            thunder_time: int("thunder_time"),
            raining: int("raining") != 0,
            thundering: int("thundering") != 0,
        }
    }

    /// The `data` compound of `weather.dat`.
    pub fn save(&self) -> Compound {
        let mut out = Compound::with_capacity(5);
        out.insert("clear_weather_time", Tag::Int(self.clear_weather_time));
        out.insert("rain_time", Tag::Int(self.rain_time));
        out.insert("thunder_time", Tag::Int(self.thunder_time));
        out.insert("raining", Tag::Byte(i8::from(self.raining)));
        out.insert("thundering", Tag::Byte(i8::from(self.thundering)));
        out
    }
}

/// Where a weather packet goes.
#[derive(Clone, Debug, PartialEq)]
pub enum WeatherPacket {
    /// To the players of this world.
    World(GameEvent),
    /// To all players of the server (Vanilla broadcasts start/stop to every dimension).
    All(GameEvent),
}

/// The rain and thunder levels of one world.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LevelWeather {
    pub rain_level: f32,
    pub o_rain_level: f32,
    pub thunder_level: f32,
    pub o_thunder_level: f32,
}

impl LevelWeather {
    /// `prepareWeather`: a world that loads in rain starts at full levels.
    pub fn prepare(data: &WeatherData) -> LevelWeather {
        let mut w = LevelWeather::default();
        if data.raining {
            w.rain_level = 1.0;
            if data.thundering {
                w.thunder_level = 1.0;
            }
        }
        w
    }

    /// `getRainLevel(1.0)`.
    pub fn rain(&self) -> f32 {
        self.rain_level
    }

    /// `getThunderLevel(1.0)`.
    pub fn thunder(&self) -> f32 {
        self.thunder_level
    }

    /// `isRaining`.
    pub fn is_raining(&self) -> bool {
        self.rain_level > 0.2
    }

    /// `isThundering`.
    pub fn is_thundering(&self) -> bool {
        self.thunder_level > 0.9
    }

    /// `advanceWeatherCycle`: advances the shared timers (when `advance` is set) and this
    /// world's levels, and returns the game events to send. Worlds without weather pass
    /// `None`; their levels stay as they are.
    pub fn advance(
        &mut self,
        data: Option<&mut WeatherData>,
        advance: bool,
        random: &mut impl RandomSource,
        out: &mut Vec<WeatherPacket>,
    ) {
        let was_raining = self.is_raining();
        if let Some(data) = data {
            if advance {
                data.advance(random);
            }
            self.o_thunder_level = self.thunder_level;
            self.thunder_level += if data.thundering { 0.01 } else { -0.01 };
            self.thunder_level = self.thunder_level.clamp(0.0, 1.0);
            self.o_rain_level = self.rain_level;
            self.rain_level += if data.raining { 0.01 } else { -0.01 };
            self.rain_level = self.rain_level.clamp(0.0, 1.0);
        }
        let ev = |event, param| GameEvent { event, param };
        if self.o_rain_level != self.rain_level {
            out.push(WeatherPacket::World(ev(
                event::RAIN_LEVEL_CHANGE,
                self.rain_level,
            )));
        }
        if self.o_thunder_level != self.thunder_level {
            out.push(WeatherPacket::World(ev(
                event::THUNDER_LEVEL_CHANGE,
                self.thunder_level,
            )));
        }
        if was_raining != self.is_raining() {
            let start_stop = if was_raining {
                event::STOP_RAINING
            } else {
                event::START_RAINING
            };
            out.push(WeatherPacket::All(ev(start_stop, 0.0)));
            out.push(WeatherPacket::All(ev(
                event::RAIN_LEVEL_CHANGE,
                self.rain_level,
            )));
            out.push(WeatherPacket::All(ev(
                event::THUNDER_LEVEL_CHANGE,
                self.thunder_level,
            )));
        }
    }

    /// The weather part of `sendLevelInfo` for a player entering this world.
    pub fn join_packets(&self) -> Vec<GameEvent> {
        if !self.is_raining() {
            return Vec::new();
        }
        vec![
            GameEvent {
                event: event::START_RAINING,
                param: 0.0,
            },
            GameEvent {
                event: event::RAIN_LEVEL_CHANGE,
                param: self.rain_level,
            },
            GameEvent {
                event: event::THUNDER_LEVEL_CHANGE,
                param: self.thunder_level,
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vanillite_base::random::LegacyRandom;

    #[test]
    fn rain_fades_in_by_one_hundredth_per_tick() {
        let mut data = WeatherData::default();
        data.set(0, 6000, true, false);
        let mut level = LevelWeather::default();
        let mut random = LegacyRandom::new(1);
        let mut out = Vec::new();
        for _ in 0..21 {
            level.advance(Some(&mut data), true, &mut random, &mut out);
        }
        assert!(level.is_raining());
        assert!(out.contains(&WeatherPacket::All(GameEvent {
            event: event::START_RAINING,
            param: 0.0
        })));
        assert_eq!(data.rain_time, 6000 - 21);
    }

    #[test]
    fn clear_weather_time_blocks_rain() {
        let mut data = WeatherData::default();
        data.set(100, 0, false, false);
        let mut random = LegacyRandom::new(1);
        data.advance(&mut random);
        assert_eq!(
            (data.clear_weather_time, data.rain_time, data.thunder_time),
            (99, 1, 1)
        );
    }
}
