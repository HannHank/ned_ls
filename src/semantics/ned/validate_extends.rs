use crate::semantics::ned::prelude::*;

pub fn validate_extends<'db>(ctx: &mut ValidationCtx<'_, 'db>, scope: &ValidationScope<'_, 'db>) {
    let extends_list = scope.item.extends(ctx.db);
    for extends in extends_list {
        let extends_name = extends.name.text(ctx.db);
        let definition = crate::db::find_def(ctx.db, extends_name, &scope.arc_defs);
        if definition.is_none() {
            let message = format!(
                "Extended module '{}' not found in any imported packages.",
                extends_name
            );
            ctx.error(extends.span.start, extends.span.end, message);
        }
    }
}
