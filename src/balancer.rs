//! Балансировщик CF-доменов: у каждой пары (DC, домен) свой счёт — сглаженное
//! время WSS-рукопожатия. Пул туннелей спрашивает [`order`] и берёт лучший
//! домен первым, а провалившиеся уходят в конец очереди.
//!
//! Идея взята из `Flowseal/tg-ws-proxy` (`proxy/balancer.py` + `proxy/pool.py`),
//! но там домен на DC выбирается случайно, без замера. Здесь домен получает
//! EWMA-счёт по реальной задержке рукопожатия, поэтому «наилучший сервер»
//! выбирается по измерению, а не по удаче.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Вес свежего замера в EWMA: чем больше, тем быстрее счёт реагирует на спад.
const ALPHA: f64 = 0.3;
/// Счёт после отказа, если замеров ещё не было (секунды).
const FAIL_BASE: f64 = 20.0;
/// Во сколько раз отказ ухудшает счёт.
const FAIL_FACTOR: f64 = 2.5;
/// Потолок счёта: совсем мёртвый домен всё равно должен пробоваться снова.
const FAIL_CAP: f64 = 120.0;

#[derive(Clone, Copy, Debug)]
struct Entry {
    /// Сглаженная задержка рукопожатия, секунды.
    score: f64,
    /// Момент последнего замера, секунды от старта процесса.
    at: f64,
    /// Успешен ли последний замер (для таблицы и решений пула).
    ok: bool,
}

/// Счёт по парам (DC, домен).
#[derive(Default)]
pub struct Scores {
    entries: HashMap<(i16, String), Entry>,
}

impl Scores {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn lookup(&self, dc: i16, domain: &str, now: f64, ttl: Duration) -> Option<f64> {
        let entry = self.entries.get(&(dc, domain.to_owned()))?;
        if now - entry.at > ttl.as_secs_f64() {
            return None;
        }
        Some(entry.score)
    }

    /// Учесть удачное рукопожатие: счёт тянется к замеру с весом [`ALPHA`].
    pub fn record_success(
        &mut self,
        dc: i16,
        domain: &str,
        handshake: Duration,
        now: f64,
        ttl: Duration,
    ) {
        let rtt = handshake.as_secs_f64().clamp(0.001, FAIL_CAP);
        let key = (dc, domain.to_owned());
        let score = match self.lookup(dc, domain, now, ttl) {
            Some(prev) => ALPHA * rtt + (1.0 - ALPHA) * prev,
            None => rtt,
        };
        self.entries.insert(key, Entry { score, at: now, ok: true });
    }

    /// Учесть отказ: домен уезжает в конец очереди, но не навсегда.
    pub fn record_failure(&mut self, dc: i16, domain: &str, now: f64, ttl: Duration) {
        let key = (dc, domain.to_owned());
        let score = match self.lookup(dc, domain, now, ttl) {
            // Один отказ весит больше любой удачной задержки: успешное
            // рукопожатие укладывается в единицы секунд, а провал — это
            // потраченное впустую ожидание таймаута.
            Some(prev) => (prev.max(FAIL_BASE) * FAIL_FACTOR).min(FAIL_CAP),
            None => FAIL_BASE,
        };
        self.entries.insert(key, Entry { score, at: now, ok: false });
    }

    /// Домены для DC в порядке предпочтения: лучший счёт первым, затем
    /// незамеренные в исходном порядке.
    pub fn order(&self, dc: i16, domains: &[String], now: f64, ttl: Duration) -> Vec<String> {
        let mut scored: Vec<(f64, &String)> = Vec::new();
        let mut rest: Vec<&String> = Vec::new();
        for domain in domains {
            match self.lookup(dc, domain, now, ttl) {
                Some(score) => scored.push((score, domain)),
                None => rest.push(domain),
            }
        }
        scored.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut out: Vec<String> = scored.into_iter().map(|(_, d)| d.clone()).collect();
        out.extend(rest.into_iter().cloned());
        out
    }

    /// Первый кандидат пула: домен с лучшим свежим счётом.
    pub fn best(&self, dc: i16, now: f64, ttl: Duration) -> Option<(String, f64)> {
        self.entries
            .iter()
            .filter(|((entry_dc, _), entry)| *entry_dc == dc && now - entry.at <= ttl.as_secs_f64())
            .min_by(|a, b| {
                a.1.score
                    .partial_cmp(&b.1.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|((_, domain), entry)| (domain.clone(), entry.score))
    }

    /// Свежие замеры для отчёта: (DC, домен, счёт, успех), лучшие первыми.
    pub fn rows(&self, now: f64, ttl: Duration) -> Vec<(i16, String, f64, bool)> {
        let mut rows: Vec<(i16, String, f64, bool)> = self
            .entries
            .iter()
            .filter(|(_, entry)| now - entry.at <= ttl.as_secs_f64())
            .map(|((dc, domain), entry)| (*dc, domain.clone(), entry.score, entry.ok))
            .collect();
        rows.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then(a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
        });
        rows
    }

    /// Выбросить замеры доменов, которых больше нет в списке.
    pub fn retain_domains(&mut self, domains: &[String]) {
        self.entries
            .retain(|(_, domain), _| domains.iter().any(|d| d == domain));
    }
}

static SCORES: OnceLock<Mutex<Scores>> = OnceLock::new();
static START: OnceLock<Instant> = OnceLock::new();

/// Общий счёт процесса.
pub fn global() -> &'static Mutex<Scores> {
    SCORES.get_or_init(|| Mutex::new(Scores::new()))
}

/// Секунды от старта процесса — общая шкала времени для счёта.
pub fn now() -> f64 {
    START
        .get_or_init(Instant::now)
        .elapsed()
        .as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn domains(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_string()).collect()
    }

    const TTL: Duration = Duration::from_secs(600);

    #[test]
    fn ewma_converges_to_measured_handshake() {
        let mut scores = Scores::new();
        for _ in 0..20 {
            scores.record_success(2, "kws2.a", Duration::from_millis(1000), 0.0, TTL);
        }
        let score = scores.lookup(2, "kws2.a", 0.0, TTL).expect("замер есть");
        assert!((score - 1.0).abs() < 0.01, "счёт {score}");

        // Один медленный замер не должен сразу переставить домен в конец.
        scores.record_success(2, "kws2.a", Duration::from_millis(3000), 0.0, TTL);
        let mixed = scores.lookup(2, "kws2.a", 0.0, TTL).expect("замер есть");
        assert!(mixed > 1.0 && mixed < 3.0, "счёт {mixed}");
    }

    #[test]
    fn failure_penalises_score() {
        let mut scores = Scores::new();
        scores.record_success(2, "kws2.a", Duration::from_millis(200), 0.0, TTL);
        let good = scores.lookup(2, "kws2.a", 0.0, TTL).expect("замер есть");
        scores.record_failure(2, "kws2.a", 0.0, TTL);
        let bad = scores.lookup(2, "kws2.a", 0.0, TTL).expect("замер есть");
        assert!(bad > good * 2.0, "было {good}, стало {bad}");

        // Без прежних замеров отказ даёт базовый штраф.
        scores.record_failure(4, "kws4.b", 0.0, TTL);
        let fresh = scores.lookup(4, "kws4.b", 0.0, TTL).expect("замер есть");
        assert!((fresh - FAIL_BASE).abs() < f64::EPSILON, "счёт {fresh}");
    }

    #[test]
    fn stale_score_is_ignored() {
        let mut scores = Scores::new();
        scores.record_success(2, "kws2.a", Duration::from_millis(100), 0.0, TTL);
        assert!(scores.lookup(2, "kws2.a", TTL.as_secs_f64() + 1.0, TTL).is_none());

        let list = domains(&["kws2.a", "kws2.b"]);
        scores.record_success(2, "kws2.b", Duration::from_millis(900), 1000.0, TTL);
        let order = scores.order(2, &list, TTL.as_secs_f64() + 1.0, TTL);
        assert_eq!(order[0], "kws2.b", "свежий замер должен идти первым");
    }

    #[test]
    fn order_puts_best_score_first() {
        let mut scores = Scores::new();
        let list = domains(&["kws2.slow", "kws2.fast", "kws2.unknown"]);
        scores.record_success(2, "kws2.slow", Duration::from_millis(1500), 0.0, TTL);
        scores.record_success(2, "kws2.fast", Duration::from_millis(120), 0.0, TTL);
        let order = scores.order(2, &list, 1.0, TTL);
        assert_eq!(order[0], "kws2.fast");
        assert_eq!(order[1], "kws2.slow");
        assert_eq!(order[2], "kws2.unknown");
        assert_eq!(scores.best(2, 1.0, TTL).map(|(d, _)| d).as_deref(), Some("kws2.fast"));

        // Отказ опускает домен ниже незамеренных собратьев.
        scores.record_failure(2, "kws2.fast", 1.0, TTL);
        let order = scores.order(2, &list, 1.0, TTL);
        assert_eq!(order[0], "kws2.slow");
    }

    #[test]
    fn rows_report_only_fresh_scores() {
        let mut scores = Scores::new();
        scores.record_success(2, "kws2.a", Duration::from_millis(500), 0.0, TTL);
        scores.record_success(4, "kws4.b", Duration::from_millis(200), 0.0, TTL);
        let rows = scores.rows(0.0, TTL);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, 2);
        assert_eq!(rows[1].1, "kws4.b");
        assert!(rows.iter().all(|row| row.3), "все замеры успешны");

        // Отказ тоже попадает в таблицу, но с флагом провала.
        scores.record_failure(2, "kws2.c", 0.0, TTL);
        let rows = scores.rows(0.0, TTL);
        let failed = rows.iter().find(|row| row.1 == "kws2.c").expect("отказ в таблице");
        assert!(!failed.3);
        assert!(scores.rows(TTL.as_secs_f64() + 1.0, TTL).is_empty());
    }

    #[test]
    fn removed_domains_drop_their_scores() {
        let mut scores = Scores::new();
        scores.record_success(2, "kws2.old", Duration::from_millis(500), 0.0, TTL);
        scores.record_success(2, "kws2.new", Duration::from_millis(500), 0.0, TTL);
        scores.retain_domains(&domains(&["kws2.new"]));
        assert!(scores.lookup(2, "kws2.old", 0.0, TTL).is_none());
        assert!(scores.lookup(2, "kws2.new", 0.0, TTL).is_some());
    }
}