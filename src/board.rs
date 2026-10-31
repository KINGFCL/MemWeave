//! 工作板：模型正在做什么。目标拆成步骤，每个目标同时只有一个进行中的步骤。
//!
//! 步骤不是知识，没有证据，也不进图。检查和执行都调用 [`apply`]，所以两边不会不一致。

use serde::{Deserialize, Serialize};

use crate::memory::is_valid_id;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goal {
    pub id: String,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    /// 编号，创建后不变。插入或删除步骤时，别的步骤的编号不受影响。
    pub number: usize,
    pub text: String,
    pub status: StepStatus,
    /// 卡住的原因。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Doing,
    Done,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoardOp {
    GoalCreate {
        id: String,
        title: String,
        description: String,
        steps: Vec<String>,
    },
    GoalDone {
        id: String,
        result: String,
    },
    GoalDrop {
        id: String,
        reason: String,
    },
    StepAdd {
        goal: String,
        text: String,
        after: Option<usize>,
    },
    StepStart {
        goal: String,
        number: usize,
    },
    StepDone {
        goal: String,
        number: usize,
    },
    StepBlock {
        goal: String,
        number: usize,
        reason: String,
    },
    StepRemove {
        goal: String,
        number: usize,
    },
}

impl BoardOp {
    /// 写进时间线的一句话。
    pub fn summary(&self) -> String {
        match self {
            Self::GoalCreate { id, title, .. } => format!("goal create {id} \"{title}\""),
            Self::GoalDone { id, result } => format!("goal done {id}：{result}"),
            Self::GoalDrop { id, reason } => format!("goal drop {id}：{reason}"),
            Self::StepAdd { goal, text, .. } => format!("step add {goal} \"{text}\""),
            Self::StepStart { goal, number } => format!("step start {goal} {number}"),
            Self::StepDone { goal, number } => format!("step done {goal} {number}"),
            Self::StepBlock {
                goal,
                number,
                reason,
            } => format!("step block {goal} {number}：{reason}"),
            Self::StepRemove { goal, number } => format!("step remove {goal} {number}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardError {
    pub code: &'static str,
    pub message: String,
    pub hint: Option<String>,
}

fn error<T>(code: &'static str, message: String) -> Result<T, BoardError> {
    Err(BoardError {
        code,
        message,
        hint: None,
    })
}

fn nonblank(value: &str, what: &str) -> Result<(), BoardError> {
    if value.trim().is_empty() {
        return error("E206", format!("{what}不能为空"));
    }
    Ok(())
}

fn goal_mut<'a>(goals: &'a mut [Goal], id: &str) -> Result<&'a mut Goal, BoardError> {
    let known: Vec<&str> = goals.iter().map(|goal| goal.id.as_str()).collect();
    if !known.contains(&id) {
        return Err(BoardError {
            code: "E218",
            message: format!("工作板上没有目标 {id}"),
            hint: Some(if known.is_empty() {
                "现在没有进行中的目标，用 goal create 新建".to_owned()
            } else {
                format!("进行中的目标：{}", known.join("、"))
            }),
        });
    }
    Ok(goals
        .iter_mut()
        .find(|goal| goal.id == id)
        .expect("上面确认过目标存在"))
}

fn step_index(goal: &Goal, number: usize) -> Result<usize, BoardError> {
    goal.steps
        .iter()
        .position(|step| step.number == number)
        .ok_or_else(|| BoardError {
            code: "E220",
            message: format!("目标 {} 没有第 {number} 步", goal.id),
            hint: Some(format!(
                "现有的步骤：{}",
                goal.steps
                    .iter()
                    .map(|step| step.number.to_string())
                    .collect::<Vec<_>>()
                    .join("、")
            )),
        })
}

/// 没有进行中的步骤时，第一个还没开始的步骤开始。返回当前步骤的说明。
fn advance(goal: &mut Goal) -> String {
    if !goal
        .steps
        .iter()
        .any(|step| step.status == StepStatus::Doing)
        && let Some(step) = goal
            .steps
            .iter_mut()
            .find(|step| step.status == StepStatus::Pending)
    {
        step.status = StepStatus::Doing;
    }
    current(goal)
}

fn current(goal: &Goal) -> String {
    if let Some(step) = goal
        .steps
        .iter()
        .find(|step| step.status == StepStatus::Doing)
    {
        return format!("当前第 {} 步：{}", step.number, step.text);
    }
    if goal
        .steps
        .iter()
        .any(|step| step.status == StepStatus::Blocked)
    {
        return "没有进行中的步骤，有步骤卡住了".to_owned();
    }
    format!("所有步骤都完成了，结束时写 goal done {}", goal.id)
}

/// 执行一个操作，返回给模型看的结果。出错时工作板不变。
pub fn apply(goals: &mut Vec<Goal>, op: &BoardOp) -> Result<String, BoardError> {
    match op {
        BoardOp::GoalCreate {
            id,
            title,
            description,
            steps,
        } => {
            if !is_valid_id(id) {
                return error("E204", format!("目标地址 {id} 不合法"));
            }
            nonblank(title, "目标的标题")?;
            nonblank(description, "目标的描述")?;
            if goals.iter().any(|goal| goal.id == *id) {
                return error("E219", format!("目标 {id} 已经在工作板上"));
            }
            for text in steps {
                nonblank(text, "步骤")?;
            }
            let mut goal = Goal {
                id: id.clone(),
                title: title.clone(),
                description: description.clone(),
                steps: steps
                    .iter()
                    .enumerate()
                    .map(|(index, text)| Step {
                        number: index + 1,
                        text: text.clone(),
                        status: StepStatus::Pending,
                        note: None,
                    })
                    .collect(),
            };
            let current = advance(&mut goal);
            goals.push(goal);
            Ok(if steps.is_empty() {
                format!("created goal {id}\nnote: 还没有步骤，用 step add 拆分")
            } else {
                format!("created goal {id}\n{current}")
            })
        }
        BoardOp::GoalDone { id, result } => {
            nonblank(result, "目标的结果")?;
            let goal = goal_mut(goals, id)?;
            let unfinished: Vec<String> = goal
                .steps
                .iter()
                .filter(|step| step.status != StepStatus::Done)
                .map(|step| step.number.to_string())
                .collect();
            if !unfinished.is_empty() {
                return Err(BoardError {
                    code: "E221",
                    message: format!("目标 {id} 还有没完成的步骤：{}", unfinished.join("、")),
                    hint: Some(format!(
                        "完成它们，或者用 step remove 去掉；不做了就用 goal drop {id}"
                    )),
                });
            }
            goals.retain(|goal| goal.id != *id);
            Ok(format!(
                "finished goal {id}\nnote: 学到了以后用得上的东西，就写成经验节点"
            ))
        }
        BoardOp::GoalDrop { id, reason } => {
            nonblank(reason, "放弃的原因")?;
            goal_mut(goals, id)?;
            goals.retain(|goal| goal.id != *id);
            Ok(format!("dropped goal {id}"))
        }
        BoardOp::StepAdd { goal, text, after } => {
            nonblank(text, "步骤")?;
            let goal = goal_mut(goals, goal)?;
            let position = match after {
                Some(number) => step_index(goal, *number)? + 1,
                None => goal.steps.len(),
            };
            let number = goal.steps.iter().map(|step| step.number).max().unwrap_or(0) + 1;
            goal.steps.insert(
                position,
                Step {
                    number,
                    text: text.clone(),
                    status: StepStatus::Pending,
                    note: None,
                },
            );
            Ok(format!("added step {number}\n{}", advance(goal)))
        }
        BoardOp::StepStart { goal, number } => {
            let goal = goal_mut(goals, goal)?;
            let index = step_index(goal, *number)?;
            if goal.steps[index].status == StepStatus::Doing {
                return error("E222", format!("第 {number} 步已经在进行中"));
            }
            for step in &mut goal.steps {
                if step.status == StepStatus::Doing {
                    step.status = StepStatus::Pending;
                }
            }
            let step = &mut goal.steps[index];
            step.status = StepStatus::Doing;
            step.note = None;
            Ok(format!("started step {number}\n{}", current(goal)))
        }
        BoardOp::StepDone { goal, number } => {
            let goal = goal_mut(goals, goal)?;
            let index = step_index(goal, *number)?;
            if goal.steps[index].status == StepStatus::Done {
                return error("E222", format!("第 {number} 步已经完成了"));
            }
            let step = &mut goal.steps[index];
            step.status = StepStatus::Done;
            step.note = None;
            Ok(format!("done step {number}\n{}", advance(goal)))
        }
        BoardOp::StepBlock {
            goal,
            number,
            reason,
        } => {
            nonblank(reason, "卡住的原因")?;
            let goal = goal_mut(goals, goal)?;
            let index = step_index(goal, *number)?;
            if goal.steps[index].status == StepStatus::Done {
                return error("E222", format!("第 {number} 步已经完成了，不能标记卡住"));
            }
            let step = &mut goal.steps[index];
            step.status = StepStatus::Blocked;
            step.note = Some(reason.clone());
            Ok(format!("blocked step {number}\n{}", current(goal)))
        }
        BoardOp::StepRemove { goal, number } => {
            let goal = goal_mut(goals, goal)?;
            let index = step_index(goal, *number)?;
            goal.steps.remove(index);
            Ok(format!("removed step {number}\n{}", advance(goal)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_steps() -> Vec<Goal> {
        let mut goals = Vec::new();
        apply(
            &mut goals,
            &BoardOp::GoalCreate {
                id: "g".into(),
                title: "目标".into(),
                description: "描述".into(),
                steps: vec!["一".into(), "二".into(), "三".into()],
            },
        )
        .unwrap();
        goals
    }

    fn statuses(goals: &[Goal]) -> Vec<(usize, StepStatus)> {
        goals[0]
            .steps
            .iter()
            .map(|s| (s.number, s.status))
            .collect()
    }

    fn done(number: usize) -> BoardOp {
        BoardOp::StepDone {
            goal: "g".into(),
            number,
        }
    }

    #[test]
    fn finishing_a_step_starts_the_next() {
        use StepStatus::{Doing, Done, Pending};
        let mut goals = with_steps();
        assert_eq!(statuses(&goals), [(1, Doing), (2, Pending), (3, Pending)]);

        let text = apply(&mut goals, &done(1)).unwrap();

        assert!(text.contains("当前第 2 步：二"), "{text}");
        assert_eq!(statuses(&goals), [(1, Done), (2, Doing), (3, Pending)]);
        apply(&mut goals, &done(2)).unwrap();
        let text = apply(&mut goals, &done(3)).unwrap();
        assert!(text.contains("所有步骤都完成了"), "{text}");
    }

    #[test]
    fn only_one_step_is_in_progress() {
        use StepStatus::{Doing, Pending};
        let mut goals = with_steps();

        let start = BoardOp::StepStart {
            goal: "g".into(),
            number: 3,
        };
        apply(&mut goals, &start).unwrap();

        assert_eq!(statuses(&goals), [(1, Pending), (2, Pending), (3, Doing)]);
        assert_eq!(apply(&mut goals, &start).unwrap_err().code, "E222");
    }

    #[test]
    fn numbers_stay_stable_when_steps_change() {
        let mut goals = with_steps();

        apply(
            &mut goals,
            &BoardOp::StepAdd {
                goal: "g".into(),
                text: "插入".into(),
                after: Some(1),
            },
        )
        .unwrap();
        apply(
            &mut goals,
            &BoardOp::StepRemove {
                goal: "g".into(),
                number: 2,
            },
        )
        .unwrap();

        let numbers: Vec<usize> = goals[0].steps.iter().map(|s| s.number).collect();
        assert_eq!(numbers, [1, 4, 3]);
    }

    #[test]
    fn blocked_steps_wait_and_can_be_restarted() {
        let mut goals = with_steps();

        let text = apply(
            &mut goals,
            &BoardOp::StepBlock {
                goal: "g".into(),
                number: 1,
                reason: "等用户确认".into(),
            },
        )
        .unwrap();

        assert!(text.contains("有步骤卡住了"), "{text}");
        assert_eq!(goals[0].steps[0].note.as_deref(), Some("等用户确认"));
        apply(
            &mut goals,
            &BoardOp::StepStart {
                goal: "g".into(),
                number: 1,
            },
        )
        .unwrap();
        assert_eq!(goals[0].steps[0].note, None);
    }

    #[test]
    fn a_goal_with_unfinished_steps_cannot_be_finished() {
        let mut goals = with_steps();
        let finish = BoardOp::GoalDone {
            id: "g".into(),
            result: "结果".into(),
        };

        assert_eq!(apply(&mut goals, &finish).unwrap_err().code, "E221");
        assert_eq!(goals.len(), 1);

        for number in 1..=3 {
            apply(&mut goals, &done(number)).unwrap();
        }
        apply(&mut goals, &finish).unwrap();
        assert!(goals.is_empty());
    }

    #[test]
    fn mistakes_are_reported_and_change_nothing() {
        let mut goals = with_steps();
        let before = goals.clone();
        let cases = [
            (done(9), "E220"),
            (
                BoardOp::StepDone {
                    goal: "没有".into(),
                    number: 1,
                },
                "E218",
            ),
            (
                BoardOp::GoalCreate {
                    id: "g".into(),
                    title: "t".into(),
                    description: "d".into(),
                    steps: Vec::new(),
                },
                "E219",
            ),
            (
                BoardOp::GoalCreate {
                    id: "../x".into(),
                    title: "t".into(),
                    description: "d".into(),
                    steps: Vec::new(),
                },
                "E204",
            ),
            (
                BoardOp::GoalCreate {
                    id: "新".into(),
                    title: "t".into(),
                    description: " ".into(),
                    steps: Vec::new(),
                },
                "E206",
            ),
            (
                BoardOp::GoalDrop {
                    id: "g".into(),
                    reason: " ".into(),
                },
                "E206",
            ),
        ];

        for (op, code) in cases {
            assert_eq!(apply(&mut goals, &op).unwrap_err().code, code, "{op:?}");
            assert_eq!(goals, before);
        }
    }
}
