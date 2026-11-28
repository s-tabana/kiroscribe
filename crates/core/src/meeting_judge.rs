//! 会議の開始・継続・終了を決める（判定表を当てはめた結果 [`Evaluation`] を、2 秒ごとに受け取る）。
//!
//! サービスごとに、次の決まりで「参加中」かを決める（MTG-007〜MTG-010）。
//! - 開始: 強い印が 1 つ立っている文脈があれば、その回に。強い印がなく、中・弱い印が合わせて 2 つ立っている文脈があれば、
//!   その組み合わせが [`JudgeConfig::combination_ms`] 続き、その間に窓の中の部品を [`JudgeConfig::combination_scans`] 回以上
//!   調べたとき。打ち消しの印のある文脈では始めない
//! - 継続: 強い印か組み合わせ（開始のあとは「開始のあとだけ」の印も数える）がある間
//! - 終了: 強い印も組み合わせもない状態が [`JudgeConfig::end_after_ms`] 続いたとき。会議の文脈に打ち消しの印が出たら、すぐ
//!
//! 録音するのは 1 つの会議だけなので、参加中のサービスのうち 1 つを「いまの会議」として開始・終了を知らせる。
//! いまの会議が終わるか、終了の猶予に入ったときに、ほかに参加中のサービスがあれば、そちらに切り替える。

use serde::{Deserialize, Serialize};

use crate::meeting_rules::{Context, Evaluation, Held, Kind, Rules, ServiceEvaluation, Strength};
use crate::meetings::{MeetingApp, MeetingEvent};

/// 開始・終了を決める時間と回数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JudgeConfig {
    pub end_after_ms: i64,
    pub combination_ms: i64,
    pub combination_scans: u32,
}

impl JudgeConfig {
    pub fn from_rules(rules: &Rules) -> Self {
        JudgeConfig {
            end_after_ms: rules.end_after_ms,
            combination_ms: rules.combination_ms,
            combination_scans: rules.combination_scans,
        }
    }
}

/// 文脈の識別（窓の文脈なら窓、デスクトップの会議アプリの文脈なら `window` が `None`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextKey {
    pub app: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u64>,
}

impl ContextKey {
    fn of(context: &Context) -> Self {
        ContextKey { app: context.app.clone(), window: context.window }
    }

    pub fn describe(&self) -> String {
        match self.window {
            Some(_) => format!("{} の窓", self.app),
            None => format!("{}（アプリ）", self.app),
        }
    }
}

/// 開始・終了の理由。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Reason {
    /// 強い印で開始した
    Strong { signals: Vec<String>, context: ContextKey },
    /// 中・弱い印の組み合わせで開始した
    Combination { signals: Vec<String>, context: ContextKey },
    /// 強い印も組み合わせもない状態が続いた
    Lost { seconds: i64 },
    /// 会議の文脈に打ち消しの印が出た
    Veto { signal: String, context: ContextKey },
    /// 別のサービスの会議に切り替えた
    Switched { to: MeetingApp },
}

impl Reason {
    /// ログ・記録に書く説明。
    pub fn describe(&self) -> String {
        match self {
            Reason::Strong { signals, context } => format!("強い印（{}）: {}", context.describe(), signals.join("・")),
            Reason::Combination { signals, context } => {
                format!("中・弱い印の組み合わせ（{}）: {}", context.describe(), signals.join("・"))
            }
            Reason::Lost { seconds } => format!("強い印も組み合わせもない状態が {seconds} 秒続いた"),
            Reason::Veto { signal, context } => format!("打ち消しの印（{}）: {signal}", context.describe()),
            Reason::Switched { to } => format!("{} の会議に切り替えた", to.label()),
        }
    }
}

/// 知らせる出来事と、その理由。
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub event: MeetingEvent,
    pub reason: Reason,
}

/// サービス 1 つの判定（ログに書く）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ServiceState {
    /// 印が立っていない
    None,
    /// 印は立っているが、開始の条件を満たしていない（打ち消しの印があるときを含む）
    Waiting,
    /// 組み合わせで、開始の確定を待っている
    Confirming,
    /// 参加中
    Joined,
    /// 参加中だったが、強い印も組み合わせもなく、終了の猶予の間
    Grace,
}

impl ServiceState {
    pub fn label(self) -> &'static str {
        match self {
            ServiceState::None => "なし",
            ServiceState::Waiting => "待機",
            ServiceState::Confirming => "確定待ち",
            ServiceState::Joined => "参加中",
            ServiceState::Grace => "猶予の間",
        }
    }
}

/// サービス 1 つの、この回の判定の様子（ログに書く）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStatus {
    pub app: MeetingApp,
    pub state: ServiceState,
    /// いちばん開始に近い文脈で立っている印（「退出ボタン（強）」の形）
    pub signals: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextKey>,
}

/// 開始・終了を確定するまでの段階（いまの会議について。ログに書く）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// 会議は見えていない
    Idle,
    /// 組み合わせで、開始の確定を待っている
    Confirming(MeetingApp),
    /// 会議が続いている
    Active(MeetingApp),
    /// 強い印も組み合わせもなく、終了の猶予の間
    Fading(MeetingApp),
}

impl Phase {
    /// 段階の短い名前（ログに書く）。
    pub fn name(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Confirming(_) => "confirming",
            Phase::Active(_) => "active",
            Phase::Fading(_) => "fading",
        }
    }

    pub fn app(self) -> Option<MeetingApp> {
        match self {
            Phase::Idle => None,
            Phase::Confirming(app) | Phase::Active(app) | Phase::Fading(app) => Some(app),
        }
    }
}

#[derive(Debug, Clone)]
struct Joined {
    since: i64,
    lost_at: Option<i64>,
    context: ContextKey,
    reason: Reason,
}

#[derive(Debug, Clone)]
struct Pending {
    since: i64,
    scans: u32,
    context: ContextKey,
}

#[derive(Debug, Clone)]
struct State {
    app: MeetingApp,
    joined: Option<Joined>,
    pending: Option<Pending>,
    status: ServiceStatus,
}

/// 文脈 1 つの見立て。
struct Candidate<'a> {
    context: &'a Context,
    counted: Vec<&'a Held>,
    veto: Option<&'a Held>,
}

impl Candidate<'_> {
    fn key(&self) -> ContextKey {
        ContextKey::of(self.context)
    }

    fn strong(&self) -> bool {
        self.veto.is_none() && self.counted.iter().any(|held| held.strength == Strength::Strong)
    }

    fn combination(&self) -> bool {
        self.veto.is_none()
            && self.counted.iter().filter(|held| matches!(held.strength, Strength::Medium | Strength::Weak)).count() >= 2
    }

    fn names(&self, pick: &dyn Fn(Strength) -> bool) -> Vec<String> {
        self.counted.iter().filter(|held| pick(held.strength)).map(|held| held.name.clone()).collect()
    }

    fn labels(&self) -> Vec<String> {
        self.counted.iter().map(|held| held.label()).collect()
    }
}

/// サービス 1 つの文脈を見立てる。窓の文脈は、窓の印か部品の印が数えられるものだけ。
fn candidates(service: Option<&ServiceEvaluation>, joined: bool) -> Vec<Candidate<'_>> {
    let Some(service) = service else { return Vec::new() };
    service
        .contexts
        .iter()
        .filter_map(|context| {
            let counted: Vec<&Held> = context.signals.iter().filter(|held| joined || !held.after_start).collect();
            if context.window.is_some() && !counted.iter().any(|held| held.kind != Kind::Session) {
                return None;
            }
            let veto = counted.iter().copied().find(|held| held.strength == Strength::Veto);
            Some(Candidate { context, counted, veto })
        })
        .collect()
}

/// 条件に合う文脈のうち、前回と同じ文脈があればそれを、なければ最初のものを選ぶ。
fn pick<'a, 'b>(list: &'b [Candidate<'a>], test: &dyn Fn(&Candidate) -> bool, prefer: Option<&ContextKey>) -> Option<&'b Candidate<'a>> {
    let matching: Vec<&Candidate> = list.iter().filter(|candidate| test(candidate)).collect();
    matching
        .iter()
        .copied()
        .find(|candidate| prefer.is_some_and(|key| candidate.key() == *key))
        .or_else(|| matching.first().copied())
}

/// サービスごとの判定を覚えておき、いまの会議の開始・終了を知らせる。
#[derive(Debug, Clone)]
pub struct Judge {
    pub config: JudgeConfig,
    states: Vec<State>,
    primary: Option<MeetingApp>,
}

impl Judge {
    pub fn new(rules: &Rules) -> Self {
        let states = rules
            .services
            .iter()
            .map(|service| State {
                app: service.app,
                joined: None,
                pending: None,
                status: ServiceStatus { app: service.app, state: ServiceState::None, signals: Vec::new(), context: None },
            })
            .collect();
        Judge { config: JudgeConfig::from_rules(rules), states, primary: None }
    }

    /// 1 回分を進め、知らせる出来事を返す（終了を先に、開始をあとに並べる）。
    pub fn tick(&mut self, evaluation: &Evaluation, now: i64) -> Vec<Decision> {
        let config = self.config;
        let mut ended: Vec<(MeetingApp, Reason)> = Vec::new();
        for state in &mut self.states {
            let joined = state.joined.is_some();
            let list = candidates(evaluation.service(state.app), joined);
            let prefer = state.joined.as_ref().map(|j| j.context.clone()).or_else(|| state.pending.as_ref().map(|p| p.context.clone()));
            let strong = pick(&list, &|c| c.strong(), prefer.as_ref());
            let combination = pick(&list, &|c| c.combination(), prefer.as_ref());
            let best = strong.or(combination);
            let shown = best.or_else(|| list.first());
            let mut status = ServiceState::None;
            if let Some(meeting) = &mut state.joined {
                let veto = list.iter().find(|c| c.key() == meeting.context).and_then(|c| c.veto);
                if let Some(veto) = veto {
                    ended.push((state.app, Reason::Veto { signal: veto.name.clone(), context: meeting.context.clone() }));
                    state.joined = None;
                } else if let Some(best) = best {
                    meeting.lost_at = None;
                    meeting.context = best.key();
                    status = ServiceState::Joined;
                } else {
                    let lost = *meeting.lost_at.get_or_insert(now);
                    if now - lost >= config.end_after_ms {
                        ended.push((state.app, Reason::Lost { seconds: config.end_after_ms / 1000 }));
                        state.joined = None;
                    } else {
                        status = ServiceState::Grace;
                    }
                }
            } else if let Some(strong) = strong {
                let reason = Reason::Strong { signals: strong.names(&|s| s == Strength::Strong), context: strong.key() };
                state.joined = Some(Joined { since: now, lost_at: None, context: strong.key(), reason });
                state.pending = None;
                status = ServiceState::Joined;
            } else if let Some(combination) = combination {
                let key = combination.key();
                let scanned = u32::from(combination.context.scanned);
                let pending = match state.pending.take() {
                    Some(mut pending) if pending.context == key => {
                        pending.scans += scanned;
                        pending
                    }
                    _ => Pending { since: now, scans: scanned, context: key.clone() },
                };
                if now - pending.since >= config.combination_ms && pending.scans >= config.combination_scans {
                    let reason = Reason::Combination {
                        signals: combination.names(&|s| matches!(s, Strength::Medium | Strength::Weak)),
                        context: key.clone(),
                    };
                    state.joined = Some(Joined { since: now, lost_at: None, context: key, reason });
                    status = ServiceState::Joined;
                } else {
                    state.pending = Some(pending);
                    status = ServiceState::Confirming;
                }
            }
            if state.joined.is_none() && !matches!(status, ServiceState::Confirming) {
                state.pending = None;
                if shown.is_some() {
                    status = ServiceState::Waiting;
                }
            }
            state.status = ServiceStatus {
                app: state.app,
                state: status,
                signals: shown.map(|c| c.labels()).unwrap_or_default(),
                context: shown.map(|c| c.key()),
            };
        }

        let mut decisions = Vec::new();
        for (app, reason) in ended {
            if self.primary == Some(app) {
                decisions.push(Decision { event: MeetingEvent::Ended(app), reason });
                self.primary = None;
            }
        }
        // いまの会議が終了の猶予に入っていて、ほかのサービスで参加中なら、そちらに切り替える
        if let Some(primary) = self.primary {
            let fading = self.state(primary).joined.as_ref().is_some_and(|meeting| meeting.lost_at.is_some());
            if let Some(next) = self.next_meeting().filter(|_| fading) {
                self.state_mut(primary).joined = None;
                self.state_mut(primary).status.state = ServiceState::None;
                decisions.push(Decision { event: MeetingEvent::Ended(primary), reason: Reason::Switched { to: next } });
                self.primary = None;
            }
        }
        if self.primary.is_none() {
            if let Some(next) = self.next_meeting() {
                let reason = self.state(next).joined.as_ref().map(|meeting| meeting.reason.clone()).expect("参加中");
                decisions.push(Decision { event: MeetingEvent::Started(next), reason });
                self.primary = Some(next);
            }
        }
        decisions
    }

    /// いまの会議のほかで、参加中（猶予の間でない）のサービスのうち、いちばん先に始まったもの。
    fn next_meeting(&self) -> Option<MeetingApp> {
        self.states
            .iter()
            .filter(|state| Some(state.app) != self.primary)
            .filter_map(|state| state.joined.as_ref().filter(|meeting| meeting.lost_at.is_none()).map(|meeting| (meeting.since, state.app)))
            .min_by_key(|(since, _)| *since)
            .map(|(_, app)| app)
    }

    fn state(&self, app: MeetingApp) -> &State {
        self.states.iter().find(|state| state.app == app).expect("判定表のサービス")
    }

    fn state_mut(&mut self, app: MeetingApp) -> &mut State {
        self.states.iter_mut().find(|state| state.app == app).expect("判定表のサービス")
    }

    /// いまの会議（開始を知らせてから、終了を知らせるまで）。
    pub fn primary(&self) -> Option<MeetingApp> {
        self.primary
    }

    /// いまの会議のほかに参加中のサービス。
    pub fn others(&self) -> Vec<MeetingApp> {
        self.states
            .iter()
            .filter(|state| state.joined.is_some() && Some(state.app) != self.primary)
            .map(|state| state.app)
            .collect()
    }

    /// サービスごとの、前の回の判定の様子。
    pub fn statuses(&self) -> Vec<&ServiceStatus> {
        self.states.iter().map(|state| &state.status).collect()
    }

    /// どれかのサービスで印が立っているか、会議中か（窓の中の部品を、間を空けずに調べる）。
    pub fn busy(&self) -> bool {
        self.states.iter().any(|state| state.joined.is_some() || state.status.state != ServiceState::None)
    }

    /// 参加中の会議の窓（ブラウザの会議では、タブを背面にしても、その窓の中を調べ続ける）。
    pub fn meeting_windows(&self) -> Vec<u64> {
        self.states.iter().filter_map(|state| state.joined.as_ref()?.context.window).collect()
    }

    /// いまの段階。
    pub fn phase(&self) -> Phase {
        if let Some(primary) = self.primary {
            return match self.state(primary).joined.as_ref() {
                Some(meeting) if meeting.lost_at.is_some() => Phase::Fading(primary),
                _ => Phase::Active(primary),
            };
        }
        match self.states.iter().find(|state| state.pending.is_some()) {
            Some(state) => Phase::Confirming(state.app),
            None => Phase::Idle,
        }
    }

    /// 開始の確定を待っているなら、待ち始めた時刻（ログに書く）。
    pub fn confirming_since(&self) -> Option<i64> {
        match self.phase() {
            Phase::Confirming(app) => self.state(app).pending.as_ref().map(|pending| pending.since),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(name: &str, strength: Strength, kind: Kind) -> Held {
        Held { name: name.into(), strength, kind, after_start: false }
    }

    fn window(app: &str, hwnd: u64, signals: Vec<Held>) -> Context {
        Context { app: app.into(), window: Some(hwnd), scanned: true, signals }
    }

    fn evaluation(services: Vec<(MeetingApp, Vec<Context>)>) -> Evaluation {
        Evaluation { services: services.into_iter().map(|(app, contexts)| ServiceEvaluation { app, contexts }).collect() }
    }

    fn events(decisions: Vec<Decision>) -> Vec<MeetingEvent> {
        decisions.into_iter().map(|d| d.event).collect()
    }

    /// 開始の確定待ち・続いている・猶予の間を、段階として読める。
    // [LOG-005][MTG-007][MTG-008]
    #[test]
    fn phase_follows_the_judgment() {
        let mut judge = Judge::new(Rules::builtin());
        let zoom = evaluation(vec![(MeetingApp::Zoom, vec![window("zoom.exe", 1, vec![held("ミーティングの窓", Strength::Strong, Kind::Window)])])]);
        assert_eq!(judge.phase(), Phase::Idle);
        assert_eq!(events(judge.tick(&zoom, 0)), vec![MeetingEvent::Started(MeetingApp::Zoom)]);
        assert_eq!(judge.phase(), Phase::Active(MeetingApp::Zoom));
        assert!(judge.tick(&Evaluation::default(), 2_000).is_empty());
        assert_eq!(judge.phase(), Phase::Fading(MeetingApp::Zoom));
        assert_eq!(judge.phase().name(), "fading");
        judge.tick(&zoom, 4_000);
        assert_eq!(judge.phase(), Phase::Active(MeetingApp::Zoom));
        assert!(judge.busy());
    }
}
