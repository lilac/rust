// Copyright 2013 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::char;

use ast;
use codemap;
use ext::base;
use ext::build::AstBuilder;

pub fn expand_syntax_ext(cx: &mut base::ExtCtxt,
                         sp: codemap::Span,
                         tts: &[ast::TokenTree]) -> base::MacResult {
    let es = match base::get_exprs_from_tts(cx, sp, tts) {
        Some(e) => e,
        None => return base::MacResult::dummy_expr()
    };
    let mut accumulator = ~"";
    for e in es.move_iter() {
        let e = cx.expand_expr(e);
        match e.node {
            ast::ExprLit(lit) => {
                match lit.node {
                    ast::LitStr(s, _) | ast::LitFloat(s, _)
                    | ast::LitFloatUnsuffixed(s) => {
                        accumulator.push_str(s);
                    }
                    ast::LitChar(c) => {
                        accumulator.push_char(char::from_u32(c).unwrap());
                    }
                    ast::LitInt(i, _) | ast::LitIntUnsuffixed(i) => {
                        accumulator.push_str(format!("{}", i));
                    }
                    ast::LitUint(u, _) => {
                        accumulator.push_str(format!("{}", u));
                    }
                    ast::LitNil => {}
                    ast::LitBool(b) => {
                        accumulator.push_str(format!("{}", b));
                    }
                    ast::LitBinary(..) => {
                        cx.span_err(e.span, "cannot concatenate a binary literal");
                    }
                }
            }
            _ => {
                cx.span_err(e.span, "expected a literal");
            }
        }
    }
    return base::MRExpr(cx.expr_str(sp, accumulator.to_managed()));
}
