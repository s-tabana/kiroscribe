//! 長い録音で、会議がまだ続いているかを確かめる（会議が終わったのに録音が続くのを防ぐ）。
//!
//! 録音が 1 時間を超えたら、5 分ごとに確かめる。会議の検出が消えている、または自分と相手の両方が
//! 10 分間話していなければ、利用者に「会議は終わりましたか？」と聞く。2 分たっても答えがなければ、
//! 録音を止める。「続ける」と答えたら、次に確かめるのは 30 分後。
//!
//! ここでは判定だけを行う（OS にも画面にも依存しないのでテストできる）。録音を止める・知らせるのはアプリの側。

/// 確かめる時間の決まり（ミリ秒）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckTiming {
    /// 録音を始めてから（一時停止を除く）、最初に確かめるまで
    pub first_check_ms: i64,
    /// 確かめる間隔
    pub interval_ms: i64,
    /// 両方が話していないと、会議が終わったとみなすまでの時間
    pub silence_ms: i64,
    /// 確認を出してから、答えがなければ止めるまで
    pub answer_ms: i64,
    /// 「続ける」と答えてから、次に確かめるまで
    pub snooze_ms: i64,
}

impl Default for CheckTiming {
    fn default() -> Self {
        Self {
            first_check_ms: 60 * 60_000,
            interval_ms: 5 * 60_000,
            silence_ms: 10 * 60_000,
            answer_ms: 2 * 60_000,
            snooze_ms: 30 * 60_000,
        }
    }
}

/// 確かめるときの録音の様子。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signals {
    /// 一時停止していた時間を除いた録音の時間
    pub elapsed_ms: i64,
    pub paused: bool,
    /// いま会議が検出されているか（開始を確定してから、終了を確定するまで）
    pub meeting_detected: bool,
    /// 自分と相手のどちらかが最後に話してからの時間（再開した時点も、話したものとして数える）
    pub quiet_ms: i64,
}

/// 会議が終わったとみなす理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndSign {
    /// 録音の間に検出していた会議が、いまは検出されていない
    MeetingGone,
    /// 両方が長く話していない
    Silence,
}

/// 確かめた結果、アプリがすること。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckAction {
    /// 確認を出す
    Ask(EndSign),
    /// 答えがないので、録音を止める
    Stop(EndSign),
    /// 答えはないが、会議が続いている様子に戻ったので、確認を取り下げる
    Withdraw,
}

/// 録音 1 回ぶんの、確かめの状態。
#[derive(Debug, Clone)]
pub struct RecordingCheck {
    timing: CheckTiming,
    /// 次に確かめる録音の時間（一時停止を除く）
    next_check_ms: i64,
    /// 確認を出した時刻（UNIX ミリ秒）
    asking_since: Option<i64>,
    /// この録音の間に会議を検出したか
    meeting_seen: bool,
}

impl Default for RecordingCheck {
    fn default() -> Self {
        Self::new(CheckTiming::default())
    }
}

impl RecordingCheck {
    pub fn new(timing: CheckTiming) -> Self {
        Self {
            timing,
            next_check_ms: timing.first_check_ms,
            asking_since: None,
            meeting_seen: false,
        }
    }

    pub fn timing(&self) -> CheckTiming {
        self.timing
    }

    /// 確認を出していれば、答えがないときに止める時刻（UNIX ミリ秒）。
    pub fn deadline(&self) -> Option<i64> {
        self.asking_since.map(|since| since + self.timing.answer_ms)
    }

    fn end_sign(&self, signals: &Signals) -> Option<EndSign> {
        if self.meeting_seen && !signals.meeting_detected {
            Some(EndSign::MeetingGone)
        } else if signals.quiet_ms >= self.timing.silence_ms {
            Some(EndSign::Silence)
        } else {
            None
        }
    }

    /// 録音の様子を受けて 1 回ぶん進める。録音の間は、数秒ごとに呼ぶ。
    pub fn tick(&mut self, signals: &Signals, now_ms: i64) -> Option<CheckAction> {
        if signals.meeting_detected {
            self.meeting_seen = true;
        }
        let sign = self.end_sign(signals);
        if let Some(since) = self.asking_since {
            if now_ms - since < self.timing.answer_ms {
                return None;
            }
            self.asking_since = None;
            return match sign {
                Some(sign) => Some(CheckAction::Stop(sign)),
                None => {
                    self.next_check_ms = signals.elapsed_ms + self.timing.interval_ms;
                    Some(CheckAction::Withdraw)
                }
            };
        }
        if signals.paused || signals.elapsed_ms < self.next_check_ms {
            return None;
        }
        match sign {
            Some(sign) => {
                self.asking_since = Some(now_ms);
                Some(CheckAction::Ask(sign))
            }
            None => {
                self.next_check_ms = signals.elapsed_ms + self.timing.interval_ms;
                None
            }
        }
    }

    /// 利用者が「続ける」と答えた。`elapsed_ms` は、そのときの録音の時間（一時停止を除く）。
    pub fn keep(&mut self, elapsed_ms: i64) {
        self.asking_since = None;
        self.next_check_ms = elapsed_ms + self.timing.snooze_ms;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;
    const T0: i64 = 1_700_000_000_000;

    fn signals(elapsed_min: i64, meeting_detected: bool, quiet_min: i64) -> Signals {
        Signals {
            elapsed_ms: elapsed_min * MIN,
            paused: false,
            meeting_detected,
            quiet_ms: quiet_min * MIN,
        }
    }

    /// 経過時間と時刻を同じだけ進めながら、1 分ごとに呼ぶ。最初に何かが起きた分と、その結果を返す。
    fn run(check: &mut RecordingCheck, from_min: i64, to_min: i64, at: impl Fn(i64) -> Signals) -> Option<(i64, CheckAction)> {
        (from_min..=to_min).find_map(|minute| check.tick(&at(minute), T0 + minute * MIN).map(|action| (minute, action)))
    }

    /// 時間の値は、設定では変えない決まった値。
    // [REC-023][REC-024][REC-025][REC-026]
    #[test]
    fn timings_have_their_product_values() {
        assert_eq!(
            CheckTiming::default(),
            CheckTiming {
                first_check_ms: 60 * MIN,
                interval_ms: 5 * MIN,
                silence_ms: 10 * MIN,
                answer_ms: 2 * MIN,
                snooze_ms: 30 * MIN,
            }
        );
    }

    /// 1 時間たつまでは、会議の検出がなくても、ずっと無音でも、確かめない。
    // [REC-023]
    #[test]
    fn nothing_is_checked_during_the_first_hour() {
        let mut check = RecordingCheck::default();
        assert_eq!(run(&mut check, 0, 59, |m| signals(m, false, m)), None);
        assert_eq!(check.tick(&signals(60, false, 60), T0 + 60 * MIN), Some(CheckAction::Ask(EndSign::Silence)));
    }

    /// 会議が続いている様子なら、1 時間を超えても確認は出さず、5 分ごとに確かめ直す。
    // [REC-023][REC-024]
    #[test]
    fn a_meeting_that_goes_on_is_checked_every_five_minutes() {
        let mut check = RecordingCheck::default();
        // 会議が検出されていて、話し声もある
        assert_eq!(run(&mut check, 0, 180, |m| signals(m, true, 0)), None);
        // 63 分に話がやんだ: 60 分・65 分・70 分の確かめでは、まだ 10 分たっていない。75 分で 12 分になる
        let mut check = RecordingCheck::default();
        let quiet_from_63 = |m: i64| signals(m, true, (m - 63).max(0));
        assert_eq!(run(&mut check, 0, 74, quiet_from_63), None);
        assert_eq!(check.tick(&quiet_from_63(75), T0 + 75 * MIN), Some(CheckAction::Ask(EndSign::Silence)));
    }

    /// 一時停止している間は確かめない。一時停止の時間は、録音の時間に入れない。
    // [REC-023]
    #[test]
    fn a_paused_recording_is_not_checked() {
        let mut check = RecordingCheck::default();
        let paused = Signals { paused: true, ..signals(90, false, 90) };
        assert_eq!(check.tick(&paused, T0), None);
        assert_eq!(check.tick(&signals(59, false, 59), T0), None, "一時停止を除いた時間が 1 時間に満たない");
        assert_eq!(check.tick(&signals(60, false, 10), T0), Some(CheckAction::Ask(EndSign::Silence)));
    }

    /// この録音の間に検出していた会議が消えていれば、話し声があっても、終わったとみなす。
    /// 会議を一度も検出していない録音（対面の会議など）は、検出がないことだけでは終わったとみなさない。
    // [REC-024]
    #[test]
    fn a_meeting_that_was_seen_and_is_gone_counts_as_ended() {
        let mut check = RecordingCheck::default();
        assert_eq!(run(&mut check, 0, 30, |m| signals(m, true, 0)), None);
        assert_eq!(run(&mut check, 31, 70, |m| signals(m, false, 0)), Some((60, CheckAction::Ask(EndSign::MeetingGone))));

        let mut in_person = RecordingCheck::default();
        assert_eq!(run(&mut in_person, 0, 300, |m| signals(m, false, 0)), None, "検出したことのない会議を、終わったとみなしている");
    }

    /// 「続ける」と答えたら、30 分たつまで確かめない。そのあとは、また 5 分ごとに確かめる。
    // [REC-025]
    #[test]
    fn keeping_on_waits_thirty_minutes() {
        let mut check = RecordingCheck::default();
        let quiet = |m: i64| signals(m, false, m);
        assert_eq!(check.tick(&quiet(60), T0 + 60 * MIN), Some(CheckAction::Ask(EndSign::Silence)));
        assert_eq!(check.deadline(), Some(T0 + 62 * MIN));
        check.keep(61 * MIN);
        assert_eq!(check.deadline(), None);
        assert_eq!(run(&mut check, 61, 90, quiet), None);
        assert_eq!(check.tick(&quiet(91), T0 + 91 * MIN), Some(CheckAction::Ask(EndSign::Silence)));
    }

    /// 答えがなければ 2 分後に止める。その時点で会議が続いている様子に戻っていれば、確認を取り下げ、5 分後にまた確かめる。
    // [REC-026]
    #[test]
    fn an_unanswered_question_stops_the_recording_after_two_minutes() {
        let mut check = RecordingCheck::default();
        let quiet = |m: i64| signals(m, false, m);
        assert_eq!(check.tick(&quiet(60), T0 + 60 * MIN), Some(CheckAction::Ask(EndSign::Silence)));
        assert_eq!(check.tick(&quiet(61), T0 + 61 * MIN), None);
        assert_eq!(check.tick(&quiet(61), T0 + 62 * MIN - 1), None);
        assert_eq!(check.tick(&quiet(62), T0 + 62 * MIN), Some(CheckAction::Stop(EndSign::Silence)));

        // 確認のあとに話し始めた
        let mut check = RecordingCheck::default();
        assert_eq!(check.tick(&quiet(60), T0 + 60 * MIN), Some(CheckAction::Ask(EndSign::Silence)));
        assert_eq!(check.tick(&signals(62, false, 0), T0 + 62 * MIN), Some(CheckAction::Withdraw));
        assert_eq!(run(&mut check, 63, 66, |m| signals(m, false, m - 62)), None);
        assert_eq!(check.tick(&signals(67, false, 10), T0 + 67 * MIN), Some(CheckAction::Ask(EndSign::Silence)));

        // 検出が消えた会議は、話し声があっても止める
        let mut check = RecordingCheck::default();
        check.tick(&signals(10, true, 0), T0 + 10 * MIN);
        assert_eq!(check.tick(&signals(60, false, 0), T0 + 60 * MIN), Some(CheckAction::Ask(EndSign::MeetingGone)));
        assert_eq!(check.tick(&signals(62, false, 0), T0 + 62 * MIN), Some(CheckAction::Stop(EndSign::MeetingGone)));

        // 一時停止していても、答えがなければ止める
        let mut check = RecordingCheck::default();
        check.tick(&quiet(60), T0 + 60 * MIN);
        let paused = Signals { paused: true, ..quiet(60) };
        assert_eq!(check.tick(&paused, T0 + 62 * MIN), Some(CheckAction::Stop(EndSign::Silence)));
    }
}
