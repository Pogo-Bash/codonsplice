//! Stack-based virtual machine for SpliceQL bytecode.
//!
//! Phase 3 status: the VM fully executes the **expression** opcodes (constants,
//! arithmetic, comparisons, short-circuit `AND`/`OR`, `NOT`, jumps) so the CLI
//! and tests can evaluate predicates end-to-end.  The **pipeline** opcodes
//! (`OPEN_SOURCE` … `WRITE_INTO`, `CALL_*`) are stubs that return
//! [`VmError::NotYetImplemented`]; wiring them to `cnvlens-core` is Phase 4.

use std::rc::Rc;

use crate::compiler::{OpCode, Program, Value};

/// A value on the VM operand stack.
///
/// [`RuntimeValue::Pending`] is a placeholder for the `Dataset`/`Cursor`/`Record`
/// handles that Phase 4 will introduce; it lets pipeline-op stubs leave the
/// stack well-typed without inventing real handles yet.
#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeValue {
    Int(i64),
    Float(f64),
    Str(Rc<str>),
    Bool(bool),
    Null,
    Pending,
}

impl RuntimeValue {
    pub fn type_name(&self) -> &'static str {
        match self {
            RuntimeValue::Int(_) => "int",
            RuntimeValue::Float(_) => "float",
            RuntimeValue::Str(_) => "string",
            RuntimeValue::Bool(_) => "bool",
            RuntimeValue::Null => "null",
            RuntimeValue::Pending => "pending",
        }
    }

    fn is_truthy(&self) -> bool {
        match self {
            RuntimeValue::Bool(b) => *b,
            RuntimeValue::Int(n) => *n != 0,
            RuntimeValue::Float(x) => *x != 0.0,
            RuntimeValue::Str(s) => !s.is_empty(),
            RuntimeValue::Null => false,
            RuntimeValue::Pending => true,
        }
    }

    fn from_const(v: &Value) -> RuntimeValue {
        match v {
            Value::Int(n) => RuntimeValue::Int(*n),
            Value::Float(x) => RuntimeValue::Float(*x),
            Value::Str(s) => RuntimeValue::Str(s.clone()),
            Value::Bool(b) => RuntimeValue::Bool(*b),
            Value::Null => RuntimeValue::Null,
        }
    }
}

/// The result of running a program.
#[derive(Debug, Clone, PartialEq)]
pub enum VmOutput {
    /// The program compiled and the (stubbed) pipeline reached `HALT`.
    Ready(Program),
    /// Textual output (used by `CALL_HEADER`; Phase 4 will produce real text).
    Text(String),
}

/// A runtime error.
#[derive(Debug, Clone, PartialEq)]
pub enum VmError {
    UnknownOpcode(u8, usize),
    StackUnderflow(usize),
    TypeMismatch {
        expected: &'static str,
        got: &'static str,
        pc: usize,
    },
    /// A pipeline/CALL opcode that is not implemented until Phase 4.
    NotYetImplemented(String),
}

impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VmError::UnknownOpcode(b, pc) => write!(f, "unknown opcode 0x{b:02X} at pc {pc}"),
            VmError::StackUnderflow(pc) => write!(f, "stack underflow at pc {pc}"),
            VmError::TypeMismatch { expected, got, pc } => {
                write!(f, "type mismatch at pc {pc}: expected {expected}, got {got}")
            }
            VmError::NotYetImplemented(op) => {
                write!(f, "opcode {op} is not implemented until Phase 4 (cnvlens bridge)")
            }
        }
    }
}

impl std::error::Error for VmError {}

/// The bytecode virtual machine.
pub struct Vm {
    program: Program,
    stack: Vec<RuntimeValue>,
    pc: usize,
}

impl Vm {
    pub fn new(program: Program) -> Self {
        Self {
            program,
            stack: Vec::new(),
            pc: 0,
        }
    }

    /// Run the main program from `pc = 0`.
    ///
    /// Expression opcodes execute fully; the first pipeline/CALL opcode returns
    /// [`VmError::NotYetImplemented`].  Reaching `HALT` (or the end of code)
    /// yields [`VmOutput::Ready`].
    pub fn run(&mut self) -> Result<VmOutput, VmError> {
        let code = self.program.code.clone();
        loop {
            if self.pc >= code.len() {
                return Ok(VmOutput::Ready(self.program.clone()));
            }
            let byte = code[self.pc];
            let op = OpCode::from_byte(byte).ok_or(VmError::UnknownOpcode(byte, self.pc))?;
            match op {
                OpCode::Halt => return Ok(VmOutput::Ready(self.program.clone())),
                // Pipeline + CALL opcodes are stubbed until Phase 4.
                OpCode::OpenSource
                | OpCode::Scan
                | OpCode::Filter
                | OpCode::Project
                | OpCode::SetParam
                | OpCode::OrderBy
                | OpCode::Limit
                | OpCode::WriteInto
                | OpCode::CallVariants
                | OpCode::CallCnv
                | OpCode::CallCoverage
                | OpCode::CallReads
                | OpCode::CallHeader => {
                    return Err(VmError::NotYetImplemented(op.name().to_string()))
                }
                _ => self.exec_expr(op)?,
            }
        }
    }

    /// Evaluate an expression-only program (as produced by
    /// [`crate::compile_expr`]) and return the value left on top of the stack.
    pub fn eval_expr(&mut self) -> Result<RuntimeValue, VmError> {
        let code = self.program.code.clone();
        loop {
            if self.pc >= code.len() {
                break;
            }
            let byte = code[self.pc];
            let op = OpCode::from_byte(byte).ok_or(VmError::UnknownOpcode(byte, self.pc))?;
            match op {
                OpCode::Halt | OpCode::RetPred => break,
                OpCode::OpenSource
                | OpCode::Scan
                | OpCode::Filter
                | OpCode::Project
                | OpCode::SetParam
                | OpCode::OrderBy
                | OpCode::Limit
                | OpCode::WriteInto
                | OpCode::CallVariants
                | OpCode::CallCnv
                | OpCode::CallCoverage
                | OpCode::CallReads
                | OpCode::CallHeader => {
                    return Err(VmError::NotYetImplemented(op.name().to_string()))
                }
                _ => self.exec_expr(op)?,
            }
        }
        self.stack
            .last()
            .cloned()
            .ok_or(VmError::StackUnderflow(self.pc))
    }

    // ── Expression interpreter ───────────────────────────────────────────────

    fn exec_expr(&mut self, op: OpCode) -> Result<(), VmError> {
        let pc0 = self.pc;
        self.pc += 1; // consume opcode byte
        match op {
            OpCode::LoadConst => {
                let idx = self.read_u16();
                let v = self
                    .program
                    .consts
                    .get(idx as usize)
                    .map(RuntimeValue::from_const)
                    .unwrap_or(RuntimeValue::Null);
                self.stack.push(v);
            }
            OpCode::LoadTrue => self.stack.push(RuntimeValue::Bool(true)),
            OpCode::LoadFalse => self.stack.push(RuntimeValue::Bool(false)),
            OpCode::Neg => {
                let a = self.pop(pc0)?;
                let r = match a {
                    RuntimeValue::Int(n) => RuntimeValue::Int(-n),
                    RuntimeValue::Float(x) => RuntimeValue::Float(-x),
                    other => {
                        return Err(VmError::TypeMismatch {
                            expected: "number",
                            got: other.type_name(),
                            pc: pc0,
                        })
                    }
                };
                self.stack.push(r);
            }
            OpCode::Not => {
                let a = self.pop(pc0)?;
                match a {
                    RuntimeValue::Bool(b) => self.stack.push(RuntimeValue::Bool(!b)),
                    other => {
                        return Err(VmError::TypeMismatch {
                            expected: "bool",
                            got: other.type_name(),
                            pc: pc0,
                        })
                    }
                }
            }
            OpCode::Add | OpCode::Sub | OpCode::Mul | OpCode::Div => {
                let b = self.pop(pc0)?;
                let a = self.pop(pc0)?;
                self.stack.push(arith(op, a, b, pc0)?);
            }
            OpCode::Eq | OpCode::Ne | OpCode::Lt | OpCode::Gt | OpCode::Le | OpCode::Ge => {
                let b = self.pop(pc0)?;
                let a = self.pop(pc0)?;
                self.stack.push(compare(op, a, b, pc0)?);
            }
            OpCode::And => {
                let jmp = self.read_u16() as usize;
                let top = self.peek(pc0)?;
                if !top.is_truthy() {
                    self.pc = jmp; // short-circuit: leave the falsey value
                } else {
                    self.pop(pc0)?; // discard left; right's value is the result
                }
            }
            OpCode::Or => {
                let jmp = self.read_u16() as usize;
                let top = self.peek(pc0)?;
                if top.is_truthy() {
                    self.pc = jmp; // short-circuit: leave the truthy value
                } else {
                    self.pop(pc0)?;
                }
            }
            OpCode::JumpIfFalse => {
                let target = self.read_u16() as usize;
                let c = self.pop(pc0)?;
                if !c.is_truthy() {
                    self.pc = target;
                }
            }
            OpCode::Jump => {
                let target = self.read_u16() as usize;
                self.pc = target;
            }
            // Opcodes requiring a record/runtime context arrive in Phase 4.
            OpCode::LoadField
            | OpCode::GetField
            | OpCode::Index
            | OpCode::LoadWildcard
            | OpCode::CallFn => {
                return Err(VmError::NotYetImplemented(op.name().to_string()))
            }
            other => return Err(VmError::NotYetImplemented(other.name().to_string())),
        }
        Ok(())
    }

    fn read_u16(&mut self) -> u16 {
        let v = u16::from_le_bytes([self.program.code[self.pc], self.program.code[self.pc + 1]]);
        self.pc += 2;
        v
    }

    fn pop(&mut self, pc: usize) -> Result<RuntimeValue, VmError> {
        self.stack.pop().ok_or(VmError::StackUnderflow(pc))
    }

    fn peek(&self, pc: usize) -> Result<RuntimeValue, VmError> {
        self.stack.last().cloned().ok_or(VmError::StackUnderflow(pc))
    }
}

/// Arithmetic: integer when both operands are integers, otherwise float.
fn arith(op: OpCode, a: RuntimeValue, b: RuntimeValue, pc: usize) -> Result<RuntimeValue, VmError> {
    use RuntimeValue::{Float, Int};
    let num = |v: &RuntimeValue| -> Option<f64> {
        match v {
            Int(n) => Some(*n as f64),
            Float(x) => Some(*x),
            _ => None,
        }
    };
    match (&a, &b) {
        (Int(x), Int(y)) => {
            let r = match op {
                OpCode::Add => x.wrapping_add(*y),
                OpCode::Sub => x.wrapping_sub(*y),
                OpCode::Mul => x.wrapping_mul(*y),
                OpCode::Div => {
                    if *y == 0 {
                        return Err(VmError::TypeMismatch {
                            expected: "nonzero divisor",
                            got: "zero",
                            pc,
                        });
                    }
                    x.wrapping_div(*y)
                }
                _ => unreachable!(),
            };
            Ok(Int(r))
        }
        _ => {
            let (x, y) = match (num(&a), num(&b)) {
                (Some(x), Some(y)) => (x, y),
                _ => {
                    let bad = if num(&a).is_none() { &a } else { &b };
                    return Err(VmError::TypeMismatch {
                        expected: "number",
                        got: bad.type_name(),
                        pc,
                    });
                }
            };
            let r = match op {
                OpCode::Add => x + y,
                OpCode::Sub => x - y,
                OpCode::Mul => x * y,
                OpCode::Div => x / y,
                _ => unreachable!(),
            };
            Ok(Float(r))
        }
    }
}

/// Comparison: `EQ`/`NE` work on any matching pair; the ordering operators
/// require numbers.
fn compare(op: OpCode, a: RuntimeValue, b: RuntimeValue, pc: usize) -> Result<RuntimeValue, VmError> {
    use RuntimeValue::{Bool, Float, Int};
    let num = |v: &RuntimeValue| -> Option<f64> {
        match v {
            Int(n) => Some(*n as f64),
            Float(x) => Some(*x),
            _ => None,
        }
    };

    if matches!(op, OpCode::Eq | OpCode::Ne) {
        let eq = a == b;
        return Ok(Bool(if op == OpCode::Eq { eq } else { !eq }));
    }

    let (x, y) = match (num(&a), num(&b)) {
        (Some(x), Some(y)) => (x, y),
        _ => {
            let bad = if num(&a).is_none() { &a } else { &b };
            return Err(VmError::TypeMismatch {
                expected: "number",
                got: bad.type_name(),
                pc,
            });
        }
    };
    let r = match op {
        OpCode::Lt => x < y,
        OpCode::Gt => x > y,
        OpCode::Le => x <= y,
        OpCode::Ge => x >= y,
        _ => unreachable!(),
    };
    Ok(Bool(r))
}
