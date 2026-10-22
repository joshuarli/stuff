//! Rust option matching over borrowed Python strings.
//! Results use Python allocations directly; no Rust heap or runtime is retained.
#![no_std]
#![allow(non_camel_case_types, non_snake_case)]

use core::cell::UnsafeCell;
use core::ffi::{c_char, c_int, c_void};
use core::ptr;

type Py_ssize_t = isize;

#[repr(C)]
struct PyObject {
    ob_refcnt: Py_ssize_t,
    ob_type: *mut PyTypeObject,
}

#[repr(C)]
struct PyTypeObject {
    _opaque: [u8; 0],
}

#[repr(C)]
union PyMethodDefFuncPointer {
    PyCFunctionFast: unsafe extern "C" fn(
        slf: *mut PyObject,
        args: *mut *mut PyObject,
        nargs: Py_ssize_t,
    ) -> *mut PyObject,
    void: *mut c_void,
}

#[repr(C)]
struct PyMethodDef {
    ml_name: *mut c_char,
    ml_meth: PyMethodDefFuncPointer,
    ml_flags: c_int,
    ml_doc: *mut c_char,
}

unsafe impl Sync for PyMethodDef {}

#[repr(C)]
struct PyModuleDef_Base {
    ob_base: PyObject,
    m_init: Option<unsafe extern "C" fn() -> *mut PyObject>,
    m_index: Py_ssize_t,
    m_copy: *mut PyObject,
}

#[repr(C)]
struct PyModuleDef {
    m_base: PyModuleDef_Base,
    m_name: *const c_char,
    m_doc: *const c_char,
    m_size: Py_ssize_t,
    m_methods: *mut PyMethodDef,
    m_slots: *mut PyModuleDef_Slot,
    m_traverse: Option<unsafe extern "C" fn(
        *mut PyObject,
        unsafe extern "C" fn(*mut PyObject, *mut c_void) -> c_int,
        *mut c_void,
    ) -> c_int>,
    m_clear: Option<extern "C" fn(*mut PyObject) -> c_int>,
    m_free: Option<extern "C" fn(*mut c_void)>,
}

const METH_FASTCALL: c_int = 0x0080;
/// `_Py_IMMORTAL_INITIAL_REFCNT | ((_Py_STATICALLY_ALLOCATED_FLAG |
/// _Py_IMMORTAL_FLAGS) << 48)` for the 64-bit GIL-enabled build.
const STATIC_IMMORTAL_REFCNT: Py_ssize_t = (3_isize << 30) | (5_isize << 48);

#[repr(C)]
struct PyModuleDef_Slot {
    slot: c_int,
    value: *mut c_void,
}

const PY_MOD_MULTIPLE_INTERPRETERS_SLOT: c_int = 86;
const PY_MOD_PER_INTERPRETER_GIL_SUPPORTED: *mut c_void = 2usize as *mut c_void;

#[cfg_attr(target_vendor = "apple", link(name = "System"))]
unsafe extern "C" {
    static mut PyExc_TypeError: *mut PyObject;
    static mut _Py_NoneStruct: PyObject;
    fn PyModuleDef_Init(module: *mut PyModuleDef) -> *mut PyObject;
    fn PyErr_NoMemory() -> *mut PyObject;
    fn PyErr_SetString(exception: *mut PyObject, message: *const c_char);
    fn PyList_New(size: Py_ssize_t) -> *mut PyObject;
    fn PyList_Append(list: *mut PyObject, item: *mut PyObject) -> c_int;
    fn PyTuple_New(size: Py_ssize_t) -> *mut PyObject;
    fn PyTuple_Size(tuple: *mut PyObject) -> Py_ssize_t;
    fn PyTuple_GetItem(tuple: *mut PyObject, index: Py_ssize_t) -> *mut PyObject;
    fn PyTuple_SetItem(tuple: *mut PyObject, index: Py_ssize_t, item: *mut PyObject) -> c_int;
    fn PyObject_IsTrue(object: *mut PyObject) -> c_int;
    fn PyUnicode_AsUTF8AndSize(object: *mut PyObject, size: *mut Py_ssize_t) -> *const c_char;
    fn PyUnicode_FromStringAndSize(text: *const c_char, size: Py_ssize_t) -> *mut PyObject;
    fn Py_NewRef(object: *mut PyObject) -> *mut PyObject;
    fn Py_DecRef(object: *mut PyObject);
    fn abort() -> !;
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { abort() }
}

// Arguments and configured option strings stay alive for this synchronous scan.
// Borrowing their UTF-8 avoids Rust heap allocations and allocator retention.
unsafe fn read_unicode<'a>(object: *mut PyObject) -> Option<&'a str> {
    let mut length: Py_ssize_t = 0;
    let data = unsafe { PyUnicode_AsUTF8AndSize(object, &mut length) };
    if data.is_null() || length < 0 {
        return None;
    }
    let bytes = unsafe { core::slice::from_raw_parts(data.cast::<u8>(), length as usize) };
    Some(unsafe { core::str::from_utf8_unchecked(bytes) })
}

unsafe fn new_unicode(text: &str) -> *mut PyObject {
    let Ok(length) = Py_ssize_t::try_from(text.len()) else {
        unsafe { PyErr_NoMemory() };
        return ptr::null_mut();
    };
    unsafe { PyUnicode_FromStringAndSize(text.as_ptr().cast::<c_char>(), length) }
}

unsafe fn new_candidate(
    option: *mut PyObject,
    separator: Option<&str>,
    explicit_argument: Option<&str>,
) -> *mut PyObject {
    let result = unsafe { PyTuple_New(3) };
    if result.is_null() {
        return ptr::null_mut();
    }

    let option = unsafe { Py_NewRef(option) };
    if option.is_null() {
        unsafe { Py_DecRef(result) };
        return ptr::null_mut();
    }
    if unsafe { PyTuple_SetItem(result, 0, option) } != 0 {
        unsafe { Py_DecRef(result) };
        return ptr::null_mut();
    }

    let separator = match separator {
        Some(separator) => unsafe { new_unicode(separator) },
        None => unsafe { Py_NewRef(ptr::addr_of_mut!(_Py_NoneStruct)) },
    };
    if separator.is_null() {
        unsafe { Py_DecRef(result) };
        return ptr::null_mut();
    }
    if unsafe { PyTuple_SetItem(result, 1, separator) } != 0 {
        unsafe { Py_DecRef(result) };
        return ptr::null_mut();
    }

    let explicit_argument = match explicit_argument {
        Some(argument) => unsafe { new_unicode(argument) },
        None => unsafe { Py_NewRef(ptr::addr_of_mut!(_Py_NoneStruct)) },
    };
    if explicit_argument.is_null() {
        unsafe { Py_DecRef(result) };
        return ptr::null_mut();
    }
    if unsafe { PyTuple_SetItem(result, 2, explicit_argument) } != 0 {
        unsafe { Py_DecRef(result) };
        return ptr::null_mut();
    }

    result
}

unsafe fn append_candidate(
    result: *mut PyObject,
    option: *mut PyObject,
    separator: Option<&str>,
    explicit_argument: Option<&str>,
) -> bool {
    let item = unsafe { new_candidate(option, separator, explicit_argument) };
    if item.is_null() {
        return false;
    }
    let appended = unsafe { PyList_Append(result, item) } >= 0;
    unsafe { Py_DecRef(item) };
    appended
}

unsafe fn scan_option(
    argument: &str,
    options: *mut PyObject,
    count: Py_ssize_t,
    allow_abbrev: bool,
    result: *mut PyObject,
) -> bool {
    if !argument.starts_with('-') {
        return true;
    }
    // Exact options take precedence over both attached values and abbreviations.
    for index in 0..count {
        let object = unsafe { PyTuple_GetItem(options, index) };
        let Some(option) = (unsafe { read_unicode(object) }) else {
            return false;
        };
        if option == argument {
            return unsafe { append_candidate(result, object, None, None) };
        }
    }
    let (prefix, separator, explicit_argument) = match argument.split_once('=') {
        Some((prefix, value)) => (prefix, Some("="), Some(value)),
        None => (argument, None, None),
    };
    if separator.is_some() {
        for index in 0..count {
            let object = unsafe { PyTuple_GetItem(options, index) };
            let Some(option) = (unsafe { read_unicode(object) }) else {
                return false;
            };
            if option == prefix {
                return unsafe { append_candidate(result, object, separator, explicit_argument) };
            }
        }
    }
    if argument.starts_with("--") {
        if allow_abbrev {
            for index in 0..count {
                let object = unsafe { PyTuple_GetItem(options, index) };
                let Some(option) = (unsafe { read_unicode(object) }) else {
                    return false;
                };
                if option.starts_with(prefix)
                    && !unsafe { append_candidate(result, object, separator, explicit_argument) }
                {
                    return false;
                }
            }
        }
        return true;
    }
    let Some(flag) = argument[1..].chars().next() else {
        return true;
    };
    let short_end = 1 + flag.len_utf8();
    let short_option = &argument[..short_end];
    let short_value = &argument[short_end..];
    for index in 0..count {
        let object = unsafe { PyTuple_GetItem(options, index) };
        let Some(option) = (unsafe { read_unicode(object) }) else {
            return false;
        };
        let appended = if option == short_option {
            unsafe { append_candidate(result, object, Some(""), Some(short_value)) }
        } else if allow_abbrev && option.starts_with(prefix) {
            unsafe { append_candidate(result, object, separator, explicit_argument) }
        } else {
            true
        };
        if !appended {
            return false;
        }
    }
    true
}

unsafe extern "C" fn option_candidates(
    _module: *mut PyObject,
    args: *mut *mut PyObject,
    nargs: Py_ssize_t,
) -> *mut PyObject {
    if nargs != 3 {
        unsafe {
            PyErr_SetString(
                PyExc_TypeError,
                c"option_candidates() takes exactly three arguments".as_ptr(),
            );
        }
        return ptr::null_mut();
    }
    let Some(argument) = (unsafe { read_unicode(*args) }) else {
        return ptr::null_mut();
    };
    let options = unsafe { *args.add(1) };
    let count = unsafe { PyTuple_Size(options) };
    if count < 0 {
        return ptr::null_mut();
    }
    // Validate every option before truth conversion or matching, including
    // options after an exact match, so malformed tuples keep raising errors.
    for index in 0..count {
        let object = unsafe { PyTuple_GetItem(options, index) };
        if object.is_null() || unsafe { read_unicode(object) }.is_none() {
            return ptr::null_mut();
        }
    }
    let allow_abbrev = unsafe { PyObject_IsTrue(*args.add(2)) };
    if allow_abbrev < 0 {
        return ptr::null_mut();
    }
    let result = unsafe { PyList_New(0) };
    if result.is_null() {
        return ptr::null_mut();
    }
    if !unsafe { scan_option(argument, options, count, allow_abbrev != 0, result) } {
        unsafe { Py_DecRef(result) };
        return ptr::null_mut();
    }
    result
}

extern "C" fn module_clear(_module: *mut PyObject) -> c_int {
    0
}

extern "C" fn module_free(_module: *mut c_void) {}

struct ModuleDef {
    ffi: UnsafeCell<PyModuleDef>,
    slots: [PyModuleDef_Slot; 2],
}

impl ModuleDef {
    fn init_multi_phase(&'static self) -> *mut PyObject {
        unsafe {
            (*self.ffi.get()).m_slots = self.slots.as_ptr() as *mut PyModuleDef_Slot;
            PyModuleDef_Init(self.ffi.get())
        }
    }
}

unsafe impl Sync for ModuleDef {}

static MODULE_METHODS: [PyMethodDef; 2] = [
    PyMethodDef {
        ml_name: c"option_candidates".as_ptr() as *mut c_char,
        ml_meth: PyMethodDefFuncPointer {
            PyCFunctionFast: option_candidates,
        },
        ml_flags: METH_FASTCALL,
        ml_doc: c"Find configured option tokens using Rust option matching".as_ptr()
            as *mut c_char,
    },
    PyMethodDef {
        ml_name: ptr::null_mut(),
        ml_meth: PyMethodDefFuncPointer { void: ptr::null_mut() },
        ml_flags: 0,
        ml_doc: ptr::null_mut(),
    },
];

static MODULE: ModuleDef = ModuleDef {
    ffi: UnsafeCell::new(PyModuleDef {
        m_base: PyModuleDef_Base {
            ob_base: PyObject {
                ob_refcnt: STATIC_IMMORTAL_REFCNT,
                ob_type: ptr::null_mut(),
            },
            m_init: None,
            m_index: 0,
            m_copy: ptr::null_mut(),
        },
        m_name: c"_argparse_rs".as_ptr() as *mut _,
        m_doc: c"Rust option-token scanner used by argparse".as_ptr() as *mut _,
        m_size: 0,
        m_methods: MODULE_METHODS.as_ptr() as *mut PyMethodDef,
        m_slots: ptr::null_mut(),
        m_traverse: None,
        m_clear: Some(module_clear),
        m_free: Some(module_free),
    }),
    slots: [
        PyModuleDef_Slot {
            slot: PY_MOD_MULTIPLE_INTERPRETERS_SLOT,
            value: PY_MOD_PER_INTERPRETER_GIL_SUPPORTED,
        },
        PyModuleDef_Slot {
            slot: 0,
            value: ptr::null_mut(),
        },
    ],
};

#[unsafe(no_mangle)]
pub extern "C" fn PyInit__argparse_rs() -> *mut PyObject {
    MODULE.init_multi_phase()
}
