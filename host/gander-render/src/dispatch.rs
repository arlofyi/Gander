// SPDX-License-Identifier: Apache-2.0
//! A minimal late-bound `IDispatch` client, just enough to drive Office automation
//! (`PowerPoint.Application`, `Word.Application`, `Excel.Application`).
//!
//! Office exposes its object model through `IDispatch` only, so every call is a name lookup
//! (`GetIDsOfNames`) followed by `Invoke`. [`Var`] owns a `VARIANT` and clears it on drop;
//! [`Dispatch`] wraps one object and offers property get/put and (positional or named-argument)
//! method calls. COM must already be initialised on the calling thread (STA for Office).

use std::mem::ManuallyDrop;

use windows::Win32::System::Com::{
    CLSCTX_LOCAL_SERVER, CLSIDFromProgID, CoCreateInstance, DISPATCH_FLAGS, DISPATCH_METHOD,
    DISPATCH_PROPERTYGET, DISPATCH_PROPERTYPUT, DISPPARAMS, EXCEPINFO, IDispatch,
};
use windows::Win32::System::Ole::DISPID_PROPERTYPUT;
use windows::Win32::System::Variant::{
    VARENUM, VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_BOOL, VT_BSTR, VT_DISPATCH,
    VT_EMPTY, VT_ERROR, VT_I2, VT_I4, VT_R4, VT_R8, VT_UI1, VariantClear,
};
use windows::core::{BSTR, GUID, HRESULT, PCWSTR};

/// `DISP_E_PARAMNOTFOUND`: how an omitted optional argument is passed.
const DISP_E_PARAMNOTFOUND: i32 = 0x8002_0004_u32 as i32;
/// `DISP_E_EXCEPTION`: the callee raised an error and filled in `EXCEPINFO`.
const DISP_E_EXCEPTION: HRESULT = HRESULT(0x8002_0009_u32 as i32);
/// `LOCALE_USER_DEFAULT`.
const LOCALE_USER_DEFAULT: u32 = 0x0400;
/// `VARIANT_TRUE`.
const VARIANT_TRUE: i16 = -1;

/// A failed automation call: the `HRESULT` and, when the callee supplied one, its description.
#[derive(Debug, Clone)]
pub struct DispError {
    /// The failing result code.
    pub code: HRESULT,
    /// What the application said (never file contents; may name our private scratch file).
    pub description: Option<String>,
}

impl std::fmt::Display for DispError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.description {
            Some(d) => write!(f, "{:#010x}: {d}", self.code.0 as u32),
            None => write!(f, "{:#010x}", self.code.0 as u32),
        }
    }
}

impl std::error::Error for DispError {}

impl From<windows::core::Error> for DispError {
    fn from(e: windows::core::Error) -> Self {
        DispError {
            code: e.code(),
            description: None,
        }
    }
}

/// An owned `VARIANT`.
pub struct Var(VARIANT);

impl std::fmt::Debug for Var {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Var(vt={})", self.vt().0)
    }
}

impl Drop for Var {
    fn drop(&mut self) {
        // SAFETY: the VARIANT is initialised and owned by this wrapper; clearing releases any
        // BSTR or interface it holds.
        unsafe {
            let _ = VariantClear(&mut self.0);
        }
    }
}

impl Var {
    fn with(vt: VARENUM, payload: VARIANT_0_0_0) -> Self {
        Var(VARIANT {
            Anonymous: VARIANT_0 {
                Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                    vt,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: payload,
                }),
            },
        })
    }

    /// `VT_EMPTY`.
    pub fn empty() -> Self {
        Var(VARIANT::default())
    }

    /// A 32-bit integer.
    pub fn i4(v: i32) -> Self {
        Self::with(VT_I4, VARIANT_0_0_0 { lVal: v })
    }

    /// A `VARIANT_BOOL`.
    pub fn boolean(v: bool) -> Self {
        Self::with(
            VT_BOOL,
            VARIANT_0_0_0 {
                boolVal: windows::Win32::Foundation::VARIANT_BOOL(if v { VARIANT_TRUE } else { 0 }),
            },
        )
    }

    /// A string.
    pub fn bstr(s: &str) -> Self {
        Self::with(
            VT_BSTR,
            VARIANT_0_0_0 {
                bstrVal: ManuallyDrop::new(BSTR::from(s)),
            },
        )
    }

    /// An omitted optional argument.
    #[allow(dead_code)]
    pub fn missing() -> Self {
        Self::with(
            VT_ERROR,
            VARIANT_0_0_0 {
                scode: DISP_E_PARAMNOTFOUND,
            },
        )
    }

    fn vt(&self) -> VARENUM {
        // SAFETY: every VARIANT this module builds or receives has the tagged layout; `vt` is
        // plain data at a fixed offset.
        unsafe { self.0.Anonymous.Anonymous.vt }
    }

    /// The value as an object, if it is one.
    pub fn into_dispatch(self) -> Option<Dispatch> {
        if self.vt() != VT_DISPATCH {
            return None;
        }
        // SAFETY: `vt` says the union holds an interface pointer; cloning adds a reference and
        // the original is released when `self` is dropped.
        unsafe {
            let slot: &Option<IDispatch> = &self.0.Anonymous.Anonymous.Anonymous.pdispVal;
            slot.clone().map(Dispatch)
        }
    }

    /// The value as an integer (integers and floats are accepted).
    pub fn to_i32(&self) -> Option<i32> {
        let vt = self.vt();
        // SAFETY: each branch reads the union member `vt` announces.
        unsafe {
            let u = &self.0.Anonymous.Anonymous.Anonymous;
            if vt == VT_I4 {
                Some(u.lVal)
            } else if vt == VT_I2 {
                Some(i32::from(u.iVal))
            } else if vt == VT_UI1 {
                Some(i32::from(u.bVal))
            } else if vt == VT_R4 {
                Some(u.fltVal.round() as i32)
            } else if vt == VT_R8 {
                Some(u.dblVal.round() as i32)
            } else {
                None
            }
        }
    }

    /// The value as a float (integers and floats are accepted).
    pub fn to_f64(&self) -> Option<f64> {
        let vt = self.vt();
        // SAFETY: each branch reads the union member `vt` announces.
        unsafe {
            let u = &self.0.Anonymous.Anonymous.Anonymous;
            if vt == VT_R4 {
                Some(f64::from(u.fltVal))
            } else if vt == VT_R8 {
                Some(u.dblVal)
            } else {
                self.to_i32().map(f64::from)
            }
        }
    }

    /// Whether this is `VT_EMPTY`.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.vt() == VT_EMPTY
    }
}

/// One automation object.
#[derive(Clone)]
pub struct Dispatch(IDispatch);

impl std::fmt::Debug for Dispatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Dispatch")
    }
}

impl Dispatch {
    /// Starts a new local-server instance of the application registered as `prog_id`.
    ///
    /// Some applications (PowerPoint) are single-instance: this call then returns the instance
    /// the user is already running. Callers must check for that before and after (see
    /// `office_com`).
    pub fn create(prog_id: &str) -> Result<Self, DispError> {
        let wide: Vec<u16> = prog_id.encode_utf16().chain(Some(0)).collect();
        // SAFETY: NUL-terminated ProgID; COM initialised by the caller.
        unsafe {
            let clsid = CLSIDFromProgID(PCWSTR(wide.as_ptr()))?;
            let obj: IDispatch = CoCreateInstance(&clsid, None, CLSCTX_LOCAL_SERVER)?;
            Ok(Dispatch(obj))
        }
    }

    /// Reads a property (with optional index arguments).
    pub fn get(&self, name: &str, args: &[Var]) -> Result<Var, DispError> {
        self.invoke(name, DISPATCH_PROPERTYGET, args, &[])
    }

    /// Reads a property that is itself an object.
    pub fn child(&self, name: &str) -> Result<Dispatch, DispError> {
        self.get(name, &[])?.into_dispatch().ok_or(DispError {
            code: HRESULT(0x8002_000B_u32 as i32), // DISP_E_BADINDEX stands in for "not an object"
            description: Some(format!("{name} is not an object")),
        })
    }

    /// Writes a property.
    pub fn put(&self, name: &str, value: Var) -> Result<(), DispError> {
        self.invoke(name, DISPATCH_PROPERTYPUT, &[value], &[])
            .map(|_| ())
    }

    /// Calls a method with positional arguments.
    pub fn call(&self, name: &str, args: &[Var]) -> Result<Var, DispError> {
        self.invoke(name, DISPATCH_METHOD, args, &[])
    }

    /// Calls a method with positional arguments followed by named arguments.
    pub fn call_named(
        &self,
        name: &str,
        positional: &[Var],
        named: &[(&str, Var)],
    ) -> Result<Var, DispError> {
        self.invoke(name, DISPATCH_METHOD, positional, named)
    }

    fn invoke(
        &self,
        name: &str,
        flags: DISPATCH_FLAGS,
        positional: &[Var],
        named: &[(&str, Var)],
    ) -> Result<Var, DispError> {
        let is_put = flags == DISPATCH_PROPERTYPUT;
        // Names: the member first, then each named argument.
        let wide: Vec<Vec<u16>> = std::iter::once(name)
            .chain(named.iter().map(|(n, _)| *n))
            .map(|n| n.encode_utf16().chain(Some(0)).collect())
            .collect();
        let ptrs: Vec<PCWSTR> = wide.iter().map(|w| PCWSTR(w.as_ptr())).collect();
        let mut ids = vec![0i32; ptrs.len()];
        // SAFETY: `ptrs` points at NUL-terminated strings that outlive the call; `ids` has one
        // slot per name.
        unsafe {
            self.0
                .GetIDsOfNames(
                    &GUID::zeroed(),
                    ptrs.as_ptr(),
                    ptrs.len() as u32,
                    LOCALE_USER_DEFAULT,
                    ids.as_mut_ptr(),
                )
                .map_err(|e| DispError {
                    code: e.code(),
                    description: Some(format!("unknown member {name}")),
                })?;
        }

        // Argument layout: named arguments occupy the first slots (matching `rgdispidNamedArgs`),
        // positional arguments follow in reverse order. A property put names its value
        // DISPID_PROPERTYPUT.
        let mut args: Vec<VARIANT> = Vec::new();
        let mut named_ids: Vec<i32> = Vec::new();
        for (i, (_, v)) in named.iter().enumerate() {
            args.push(bitwise_copy(v));
            named_ids.push(ids[i + 1]);
        }
        for v in positional.iter().rev() {
            args.push(bitwise_copy(v));
        }
        if is_put {
            named_ids.push(DISPID_PROPERTYPUT);
        }
        let params = DISPPARAMS {
            rgvarg: if args.is_empty() {
                std::ptr::null_mut()
            } else {
                args.as_mut_ptr()
            },
            rgdispidNamedArgs: if named_ids.is_empty() {
                std::ptr::null_mut()
            } else {
                named_ids.as_mut_ptr()
            },
            cArgs: args.len() as u32,
            cNamedArgs: named_ids.len() as u32,
        };
        let mut result = Var::empty();
        let mut excep = EXCEPINFO::default();
        let mut arg_err = 0u32;
        // SAFETY: `params` points at `args`/`named_ids`, which live until after the call; the
        // argument VARIANTs are bitwise copies owned by the caller's `Var`s (never cleared here);
        // result and exception out-parameters are valid.
        let outcome = unsafe {
            self.0.Invoke(
                ids[0],
                &GUID::zeroed(),
                LOCALE_USER_DEFAULT,
                flags,
                &params,
                Some(&mut result.0),
                Some(&mut excep),
                Some(&mut arg_err),
            )
        };
        match outcome {
            Ok(()) => Ok(result),
            Err(e) => {
                let description = (e.code() == DISP_E_EXCEPTION).then(|| {
                    let text = excep.bstrDescription.to_string();
                    // SAFETY: the callee allocated these BSTRs for us; each is released once.
                    unsafe {
                        ManuallyDrop::drop(&mut excep.bstrSource);
                        ManuallyDrop::drop(&mut excep.bstrDescription);
                        ManuallyDrop::drop(&mut excep.bstrHelpFile);
                    }
                    text
                });
                Err(DispError {
                    code: e.code(),
                    description: description.filter(|d| !d.is_empty()),
                })
            }
        }
    }
}

/// A bitwise copy of a VARIANT for use as an `Invoke` argument. The copy is never cleared: the
/// `Var` it came from still owns whatever it points to.
fn bitwise_copy(v: &Var) -> VARIANT {
    // SAFETY: VARIANT is plain data (its pointers are borrowed, not owned, by the copy) and the
    // copy is dropped without running any destructor.
    unsafe { std::mem::transmute_copy(&v.0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_round_trip_their_payloads() {
        assert_eq!(Var::i4(-7).to_i32(), Some(-7));
        assert_eq!(Var::i4(3).to_f64(), Some(3.0));
        assert_eq!(Var::boolean(true).to_i32(), None);
        assert!(Var::empty().is_empty());
        assert_eq!(Var::bstr("x").to_i32(), None);
        assert!(Var::missing().into_dispatch().is_none());
    }
}
