use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use codespan::FileId;
use crate::{ast, codegen::{CodegenConfig, CompileError}};
use crate::ast::Type;

pub struct CBackend {
    config: CodegenConfig,
    header: String,
    body: String,
    file_id: FileId,
    includes: RefCell<HashSet<&'static str>>,
    variables: RefCell<HashMap<String, String>>,
}


impl CBackend {
    pub fn new(config: CodegenConfig, file_id: FileId) -> Self {
        Self {
            config,
            header: String::new(),
            body: String::new(),
            file_id,
            includes: RefCell::new(HashSet::new()),
            variables: RefCell::new(HashMap::new()),
        }
    }

    pub fn compile(&mut self, program: &ast::Program) -> Result<(), CompileError> {
        self.emit_globals(program)?;
        self.emit_functions(program)?;
        self.emit_main_if_missing(program)?;

        self.emit_header();
        self.write_output()?;
        Ok(())
    }

    fn emit_header(&mut self) {
        self.header.push_str(&format!(
            "// Generated by Verve Compiler (target: {})\n",
            self.config.target_triple
        ));
        self.header.push_str("#include <stdio.h>\n#include <stdlib.h>\n");

        for include in self.includes.borrow().iter() {
            self.header.push_str(&format!("#include {}\n", include));
        }

        self.header.push('\n');
    }


    fn emit_globals(&mut self, program: &ast::Program) -> Result<(), CompileError> {
        for stmt in &program.stmts {
            if let ast::Stmt::Let(name, ty, expr, _) = stmt {
                if self.is_constant_expr(expr) {
                    let c_ty = self.type_to_c(ty.as_ref().unwrap_or(&Type::I32));
                    let value = self.emit_expr(expr)?;
                    self.body.push_str(&format!("{} {} = {};\n", c_ty, name, value));
                } else {
                    return Err(CompileError::CodegenError {
                        message: format!("Non-constant initializer for global '{}'", name),
                        span: Some(expr.span()),
                        file_id: self.file_id,
                    });
                }
            }
        }
        Ok(())
    }



    fn is_constant_expr(&self, expr: &ast::Expr) -> bool {
        matches!(expr, ast::Expr::Int(..) | ast::Expr::Str(..))
    }

    fn emit_main_if_missing(&mut self, program: &ast::Program) -> Result<(), CompileError> {
        if !program.functions.iter().any(|f| f.name == "main") {
            self.body.push_str("\nint main() {\n");

            for stmt in &program.stmts {
                if !matches!(stmt, ast::Stmt::Let(..)) {
                    self.emit_stmt(stmt)?;
                }
            }
            
            #[cfg(target_os = "windows")]
            self.body.push_str("    system(\"pause\");\n");
            #[cfg(not(target_os = "windows"))]
            self.body.push_str("    getchar();\n");

            self.body.push_str("    return 0;\n}\n");
        }
        Ok(())
    }

    fn emit_functions(&mut self, program: &ast::Program) -> Result<(), CompileError> {
        for func in &program.functions {
            let return_type = if func.name == "main" {
                "int".to_string()
            } else {
                self.type_to_c(&func.return_type)
            };
            let params = func.params.iter()
                .map(|(name, ty)| format!("{} {}", self.type_to_c(ty), name))
                .collect::<Vec<_>>()
                .join(", ");
            self.body.push_str(&format!("{} {}({});\n", return_type, func.name, params));
        }
        self.body.push('\n');
        
        for func in &program.functions {
            self.emit_function(func)?;
        }
        Ok(())
    }

    fn emit_function(&mut self, func: &ast::Function) -> Result<(), CompileError> {
        let return_type = if func.name == "main" {
            "int".to_string()
        } else {
            self.type_to_c(&func.return_type)
        };

        let mut param_strings = Vec::new();
        for (name, ty) in &func.params {
            let c_ty = self.type_to_c(ty);
            param_strings.push(format!("{} {}", c_ty, name));
        }
        let params = param_strings.join(", ");

        self.body.push_str(&format!("{} {}({}) {{\n", return_type, func.name, params));

        for stmt in &func.body {
            self.emit_stmt(stmt)?;
        }

        if func.name == "main" {
            #[cfg(target_os = "windows")]
            self.body.push_str("    system(\"pause\");\n");
            #[cfg(not(target_os = "windows"))]
            self.body.push_str("    getchar();\n");


            let last_is_return = func.body.last().is_some_and(|s| matches!(s, ast::Stmt::Return(..)));

            if !last_is_return {
                self.body.push_str("    return 0;\n");
            }
        } else if func.return_type == Type::Void {
            self.body.push_str("    return;\n");
        }

        self.body.push_str("}\n\n");
        Ok(())
    }

    fn emit_stmt(&mut self, stmt: &ast::Stmt) -> Result<(), CompileError> {
        match stmt {
            ast::Stmt::Let(name, ty, expr, _) => {
                let c_ty = match ty {
                    Some(t) => self.type_to_c(t),
                    None => {
                        let expr_type = if let ast::Expr::Var(var_name, _, _) = expr {
                            if var_name == "true" || var_name == "false" {
                                Type::Bool
                            } else {
                                expr.get_type()
                            }
                        } else {
                            expr.get_type()
                        };
                        if expr_type == Type::Unknown {
                            "int".to_string()
                        } else {
                            self.type_to_c(&expr_type)
                        }
                    }
                };
                let expr_code = self.emit_expr(expr)?;
                self.body.push_str(&format!("{} {} = {};\n", c_ty, name, expr_code));
                self.variables.borrow_mut().insert(name.clone(), c_ty);
            }
            ast::Stmt::Return(expr, _) => {
                let expr_code = self.emit_expr(expr)?;
                self.body.push_str(&format!("return {};\n", expr_code));
            },
            ast::Stmt::Expr(expr, _) => {
                let expr_code = self.emit_expr(expr)?;
                if expr_code.starts_with('{') {
                    self.body.push_str(&expr_code);
                } else if !expr_code.ends_with(';') {
                    self.body.push_str(&format!("{};\n", expr_code));
                } else {
                    self.body.push_str(&format!("{}\n", expr_code));
                }
            },
            ast::Stmt::While(cond, body, _) => {
                let cond_code = self.emit_expr(cond)?;
                self.body.push_str(&format!("while ({}) {{\n", cond_code));
                for stmt in body {
                    self.emit_stmt(stmt)?;
                }
                self.body.push_str("}\n");
            },
            ast::Stmt::For(init, cond, incr, body, _) => {
                self.body.push_str("for (");
                if let Some(init) = init {
                    match &**init {
                        ast::Stmt::Let(name, ty, expr, _) => {
                            let c_ty = ty.as_ref().map(|t| self.type_to_c(t))
                                .unwrap_or_else(|| "int".parse().unwrap());
                            let value = self.emit_expr(expr)?;
                            self.body.push_str(&format!("{} {} = {}", c_ty, name, value));
                        }
                        ast::Stmt::Expr(expr, _) => {
                            let code = self.emit_expr(expr)?;
                            self.body.push_str(&code);
                        }
                        _ => return Err(CompileError::CodegenError {
                            message: "Unsupported for loop initializer".to_string(),
                            span: None,
                            file_id: self.file_id,
                        }),
                    }
                }
                self.body.push_str("; ");
                
                if let Some(cond) = cond {
                    let cond_code = self.emit_expr(cond)?;
                    self.body.push_str(&format!("({})", cond_code));
                } else {
                    self.body.push('1');  
                }

                self.body.push_str("; ");
                
                if let Some(incr) = incr {
                    let incr_code = self.emit_expr(incr)?;
                    self.body.push_str(&format!("({})", incr_code));
                }

                self.body.push_str(") {\n");
                for stmt in body {
                    self.emit_stmt(stmt)?;
                }
                self.body.push_str("}\n");
            },
            _ => unimplemented!(),
        }
        Ok(())
    }

    fn emit_expr(&mut self, expr: &ast::Expr) -> Result<String, CompileError> {
        match expr {
            ast::Expr::Int(n, _, _) => Ok(n.to_string()),
            ast::Expr::BinOp(left, op, right, _span, _) => {
                let left_code = self.emit_expr(left)?;
                let right_code = self.emit_expr(right)?;
                let op_str = match op {
                    ast::BinOp::Add => "+",
                    ast::BinOp::Sub => "-",
                    ast::BinOp::Mul => "*",
                    ast::BinOp::Div => "/",
                    ast::BinOp::Gt => ">",
                    ast::BinOp::Eq => "==",
                };
                Ok(format!("({} {} {})", left_code, op_str, right_code))
            },
            ast::Expr::Assign(target, value, _, _) => {
                let target_code = self.emit_expr(target)?;
                let value_code = self.emit_expr(value)?;
                Ok(format!("({} = {})", target_code, value_code))
            },
            ast::Expr::Str(s, _, _) => Ok(format!("\"{}\"", s)),
            ast::Expr::Var(name, _, _) => {
                if name == "true" || name == "false" {
                    self.includes.borrow_mut().insert("<stdbool.h>");
                    Ok(name.clone())
                } else {
                    Ok(name.clone())
                }
            },
            ast::Expr::Print(expr, _span, _) => {
                let value = self.emit_expr(expr)?;
                let expr_ty = if let ast::Expr::Var(var_name, _, _) = &**expr {
                    self.variables.borrow()
                        .get(var_name)
                        .and_then(|c_ty| match c_ty.as_str() {
                            "int" => Some(Type::I32),
                            "bool" => Some(Type::Bool),
                            "const char*" => Some(Type::String),
                            _ => None,
                        })
                        .unwrap_or(Type::Unknown)
                } else {
                    expr.get_type()
                };


                let (format_spec, arg) = match expr_ty {
                    Type::I32 => ("%d", value),
                    Type::Bool => ("%s", format!("({} ? \"true\" : \"false\")", value)),
                    Type::String => ("%s", value),
                    Type::Pointer(_) | Type::RawPtr => {
                        self.includes.borrow_mut().insert("<inttypes.h>");
                        ("%\"PRIuPTR\"", format!("(uintptr_t){}", value))
                    },
                    _ => return Err(CompileError::CodegenError {
                        message: format!("Cannot print type {}", expr_ty),
                        span: Some(expr.span()),
                        file_id: self.file_id,
                    }),
                };
                Ok(format!("printf(\"{}\\n\", {});", format_spec, arg))
            },
            ast::Expr::Call(name, args, _, _) => {
                let mut args_code = Vec::new();
                for arg in args {
                    args_code.push(self.emit_expr(arg)?);
                }
                Ok(format!("{}({})", name, args_code.join(", ")))
            },
            ast::Expr::IntrinsicCall(name, args, span, _) => match name.as_str() {
                "__alloc" => {
                    if args.len() != 1 {
                        return Err(CompileError::CodegenError {
                            message: "__alloc expects 1 argument".to_string(),
                            span: Some(*span),
                            file_id: self.file_id,
                        });
                    }
                    let size = self.emit_expr(&args[0])?;
                    Ok(format!("malloc({})", size))
                },
                "__dealloc" => {
                    if args.len() != 1 {
                        return Err(CompileError::CodegenError {
                            message: "__dealloc expects 1 argument".to_string(),
                            span: Some(*span),
                            file_id: self.file_id,
                        });
                    }
                    let ptr = self.emit_expr(&args[0])?;
                    Ok(format!("free({})", ptr))
                }
                _ => Err(CompileError::CodegenError {
                    message: format!("Unknown intrinsic function: {}", name),
                    span: Some(*span),
                    file_id: self.file_id,
                }),
            },
            ast::Expr::SafeBlock(stmts, _span, _) => {
                let mut code = String::new();
                code.push_str("{\n");
                let mut defers = Vec::new();

                for stmt in stmts {
                    match stmt {
                        ast::Stmt::Defer(expr, _) => {
                            let expr_code = self.emit_expr(expr)?;
                            defers.push(expr_code);
                        },
                        _ => {
                            let stmt_code = self.emit_stmt_to_string(stmt)?;
                            code.push_str(&stmt_code);
                        }
                    }
                }

                for deferred in defers.into_iter().rev() {
                    code.push_str(&format!("{};\n", deferred));
                }

                code.push_str("}\n");
                Ok(code)
            },
            ast::Expr::Deref(expr, _, _) => {
                let inner = self.emit_expr(expr)?;
                Ok(format!("(*{})", inner))
            }
            ast::Expr::Cast(expr, target_ty, _, _) => {
                let expr_code = self.emit_expr(expr)?;
                let expr_type = expr.get_type();

                let target_c_ty = if expr_type.is_pointer() && *target_ty == Type::I32 {
                    self.includes.borrow_mut().insert("<stdint.h>");
                    "uintptr_t".to_string()
                } else {
                    self.type_to_c(target_ty)
                };

                Ok(format!("({})({})", target_c_ty, expr_code))
            },
            _ => Err(CompileError::CodegenError {
                message: "Unsupported expression".to_string(),
                span: Some(expr.span()),
                file_id: self.file_id,
            }),
        }
    }
    
    fn emit_stmt_to_string(&mut self, stmt: &ast::Stmt) -> Result<String, CompileError> {
        let mut buffer = String::new();
        let original_body = std::mem::replace(&mut self.body, String::new());
        self.emit_stmt(stmt)?;
        buffer = std::mem::replace(&mut self.body, original_body);
        Ok(buffer)
    }

    fn type_to_c(&self, ty: &Type) -> String {
        match ty {
            Type::I32 => "int".to_string(),
            Type::Bool => {
                self.includes.borrow_mut().insert("<stdbool.h>");
                "bool".to_string()
            },
            Type::String => "const char*".to_string(),
            Type::Void => "void".to_string(),
            Type::Pointer(inner) => {
                let inner_type = self.type_to_c(inner);
                format!("{}*", inner_type)
            },
            Type::RawPtr => "void*".to_string(),
            _ => "/* UNSUPPORTED TYPE */".to_string(),
        }
    }

    fn write_output(&self) -> Result<(), CompileError> {
        let full_output = format!("{}{}", self.header, self.body);
        std::fs::write("output.c", &full_output)?;
        Ok(())
    }
}