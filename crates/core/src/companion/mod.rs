//! 会議の相棒: 録音中の発言から資料を求める発言を見つけ、答えに使える資料を探して選ぶ。
//!
//! - [`trigger`]: 候補の発言か（PC の中だけで決める）
//! - [`candidates`]: 探し先を探して候補にし、AI に送る形を作る
//! - [`ai`]: 判定と選択（Anthropic の Messages API）

pub mod ai;
pub mod candidates;
pub mod trigger;

use std::future::Future;

pub use ai::{Found, JudgeInput, Judgement, Selection};
pub use candidates::{Candidate, CurrentNote, Period, SourceKind};

use crate::claude::ClaudeClient;
use crate::error::Result;

/// 判定と選択を行うもの（テストでは、呼ばれた回数を数える模擬に差し替える）。
pub trait Assistant {
    fn judge(&self, input: &JudgeInput) -> impl Future<Output = Result<Judgement>> + Send;
    fn select(&self, utterance: &str, target: &str, candidates: &[Candidate]) -> impl Future<Output = Result<Selection>> + Send;
}

impl Assistant for ClaudeClient {
    fn judge(&self, input: &JudgeInput) -> impl Future<Output = Result<Judgement>> + Send {
        ai::judge(self, input)
    }

    fn select(&self, utterance: &str, target: &str, candidates: &[Candidate]) -> impl Future<Output = Result<Selection>> + Send {
        ai::select(self, utterance, target, candidates)
    }
}

/// 1 つの発言への相棒の答え。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// 候補の発言ではない（AI は呼ばない）
    NotCandidate,
    /// 判定が「求めていない」（何も表示しない）
    NotRequested,
    /// 資料を探したが、候補がない・AI が「なし」を返した
    NotFound(Judgement),
    Found(Judgement, Found),
}

/// 1 つの発言について、候補かを決め、判定し、求めていれば探して選ぶ。
/// AI を呼ぶのは、候補の発言 1 件につき判定 1 回と、求めていて候補が 1 件以上あるときの選択 1 回だけ。
/// `on_requested` は、判定が「求めている」になった時点で呼ぶ（画面に「調べています」を出すため）。
pub async fn respond<A: Assistant>(
    assistant: &A,
    input: &JudgeInput,
    search: impl FnOnce(&Judgement) -> Vec<Candidate>,
    on_requested: impl FnOnce(&Judgement),
) -> Result<Outcome> {
    if !trigger::is_candidate(&input.text) {
        return Ok(Outcome::NotCandidate);
    }
    let judgement = assistant.judge(input).await?;
    if !judgement.requested {
        return Ok(Outcome::NotRequested);
    }
    on_requested(&judgement);
    let candidates = search(&judgement);
    if candidates.is_empty() {
        return Ok(Outcome::NotFound(judgement));
    }
    let selection = assistant.select(&input.text, &judgement.target, &candidates).await?;
    Ok(match ai::resolve(&selection, &candidates, &input.text) {
        Some(found) => Outcome::Found(judgement, found),
        None => Outcome::NotFound(judgement),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::transcript::Speaker;

    struct Fake {
        requested: bool,
        choice: usize,
        judged: AtomicUsize,
        selected: AtomicUsize,
    }

    impl Fake {
        fn new(requested: bool, choice: usize) -> Self {
            Self { requested, choice, judged: AtomicUsize::new(0), selected: AtomicUsize::new(0) }
        }
        fn calls(&self) -> (usize, usize) {
            (self.judged.load(Ordering::SeqCst), self.selected.load(Ordering::SeqCst))
        }
    }

    impl Assistant for Fake {
        async fn judge(&self, _input: &JudgeInput) -> Result<Judgement> {
            self.judged.fetch_add(1, Ordering::SeqCst);
            Ok(if self.requested {
                Judgement { requested: true, target: "経費精算のマニュアル".into(), terms: vec!["経費".into()], period: None }
            } else {
                Judgement::not_requested()
            })
        }
        async fn select(&self, _u: &str, _t: &str, _c: &[Candidate]) -> Result<Selection> {
            self.selected.fetch_add(1, Ordering::SeqCst);
            Ok(Selection { choice: self.choice, passage: String::new(), clues: vec![] })
        }
    }

    fn input(text: &str) -> JudgeInput {
        JudgeInput { note_title: "定例".into(), speaker: Speaker::Them, text: text.into(), previous: vec![] }
    }

    fn candidate() -> Candidate {
        Candidate {
            kind: SourceKind::Folder,
            title: "経費精算".into(),
            location: "/a/経費精算.docx".into(),
            send_location: "a/経費精算.docx".into(),
            excerpt: None,
            part: None,
            no_content: false,
            path: None,
            note_id: None,
        }
    }

    // [CMP-041][CMP-007]
    #[tokio::test]
    async fn calls_the_ai_only_when_needed() {
        let asked = "経費精算のマニュアルってどこ？";

        let fake = Fake::new(true, 1);
        let outcome = respond(&fake, &input("了解です。"), |_| vec![candidate()], |_| {}).await.unwrap();
        assert_eq!(outcome, Outcome::NotCandidate);
        assert_eq!(fake.calls(), (0, 0), "候補でない発言では AI を呼ばない");

        let fake = Fake::new(false, 1);
        let mut requested = false;
        let outcome = respond(&fake, &input(asked), |_| vec![candidate()], |_| requested = true).await.unwrap();
        assert_eq!(outcome, Outcome::NotRequested);
        assert!(!requested, "「求めていない」なら何も表示しない");
        assert_eq!(fake.calls(), (1, 0));

        let fake = Fake::new(true, 1);
        let outcome = respond(&fake, &input(asked), |_| vec![], |_| {}).await.unwrap();
        assert!(matches!(outcome, Outcome::NotFound(_)));
        assert_eq!(fake.calls(), (1, 0), "候補が 0 件なら選択を呼ばない");

        let fake = Fake::new(true, 1);
        let mut requested = false;
        let outcome = respond(&fake, &input(asked), |_| vec![candidate()], |_| requested = true).await.unwrap();
        assert!(matches!(&outcome, Outcome::Found(_, found) if found.candidate.title == "経費精算"));
        assert!(requested);
        assert_eq!(fake.calls(), (1, 1), "判定 1 回と選択 1 回");

        let fake = Fake::new(true, 0);
        let outcome = respond(&fake, &input(asked), |_| vec![candidate()], |_| {}).await.unwrap();
        assert!(matches!(outcome, Outcome::NotFound(_)), "AI が「なし」を返した");
    }
}
