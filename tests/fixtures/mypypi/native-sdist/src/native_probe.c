#include <Python.h>

#ifdef __linux__
#include <fcntl.h>
#include <sys/stat.h>
#endif

static PyObject *answer(PyObject *self, PyObject *args)
{
    (void)self;
    (void)args;
#ifdef __linux__
    struct statx stx;
    if (statx(AT_FDCWD, "/", 0, STATX_BASIC_STATS, &stx) != 0) {
        return PyErr_SetFromErrno(PyExc_OSError);
    }
#endif
    return PyLong_FromLong(42);
}

static PyMethodDef methods[] = {
    {"answer", answer, METH_NOARGS, "Return the probe answer."},
    {NULL, NULL, 0, NULL},
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "native_probe",
    NULL,
    -1,
    methods,
};

PyMODINIT_FUNC PyInit_native_probe(void)
{
    return PyModule_Create(&module);
}
