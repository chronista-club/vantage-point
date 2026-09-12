//! Chat / Console の host 交代を越える、VP session 所有の非同期質問の保管領域。
//! daemon 内のメモリのみ。native server request / 承認は保管しない（design 69）。
use super::codex_async_questions::AsyncQuestions;
use std::sync::{Arc, Mutex};

struct Parked {
    thread_id: String,
    questions: AsyncQuestions,
}

#[derive(Clone, Default)]
pub(crate) struct CodexQuestionSession(Arc<Mutex<Option<Parked>>>);

impl CodexQuestionSession {
    pub(super) fn suspend(&self, thread_id: &str, questions: &AsyncQuestions) -> AsyncQuestions {
        let questions = questions.for_handoff();
        let view = questions.waiting_for_resume();
        *self.0.lock().expect("question session lock") = Some(Parked {
            thread_id: thread_id.into(),
            questions,
        });
        view
    }

    /// 起動直後の空 snapshot で UI の下書きを消さない。別会話への移動では破棄する。
    pub(super) fn preview(&self, thread_id: Option<&str>) -> AsyncQuestions {
        let mut saved = self.0.lock().expect("question session lock");
        if let Some(parked) = saved.as_ref()
            && Some(parked.thread_id.as_str()) == thread_id
        {
            return parked.questions.waiting_for_resume();
        }
        *saved = None;
        AsyncQuestions::default()
    }

    /// 履歴と会話 ID の検証に成功した host に一度だけ渡す。
    pub(super) fn resume(&self, thread_id: &str) -> Option<AsyncQuestions> {
        self.0
            .lock()
            .expect("question session lock")
            .take()
            .filter(|parked| parked.thread_id == thread_id)
            .map(|parked| parked.questions)
    }

    /// 再開準備中にユーザーが見送った質問を、再開後に復活させない。
    pub(super) fn dismiss(&self, request_id: &str) {
        if let Some(parked) = self.0.lock().expect("question session lock").as_mut() {
            parked.questions.finish(request_id);
        }
    }
}
