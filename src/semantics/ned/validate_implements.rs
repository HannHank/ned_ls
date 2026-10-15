use crate::semantics::ned::prelude::*;

pub fn validate_implements<'db>(
    ctx: &mut ValidationCtx<'_, 'db>,
    scope: &ValidationScope<'_, 'db>,
) -> Vec<DefIndex<'db>> {
    let likes = scope.item.like(ctx.db);
    let mut likes_def = Vec::new();
    for like in likes {
        let like_name = like.name.text(ctx.db);
        let like_def = crate::db::find_def(ctx.db, like_name, &scope.arc_defs);

        match like_def {
            Some(def) => {
                likes_def.push(def);
            }
            None => {
                let message = format!(
                    "Interface '{}' not found in any imported packages.",
                    like_name
                );
                ctx.error(like.span.start, like.span.end, message);
            }
        }
    }
    likes_def
}
