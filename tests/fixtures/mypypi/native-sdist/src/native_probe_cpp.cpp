#include <Python.h>

#include <string>

static PyObject *answer(PyObject *self, PyObject *args)
{
    (void)self;
    (void)args;
    std::string value("42");
    return PyLong_FromLong(std::stol(value));
}

static PyMethodDef methods[] = {
    {"answer", answer, METH_NOARGS, "Return the probe answer."},
    {NULL, NULL, 0, NULL},
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "native_probe_cpp",
    NULL,
    -1,
    methods,
};

extern "C" PyMODINIT_FUNC PyInit_native_probe_cpp(void)
{
    return PyModule_Create(&module);
}
