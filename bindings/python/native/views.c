#define PY_SSIZE_T_CLEAN
#include "plasmite.h"
#include <Python.h>
#include <stdint.h>
#include <string.h>
#ifdef _WIN32
#include <windows.h>
#else
#include <dlfcn.h>
#endif

/* Python owns wrappers; libplasmite owns one independent immutable snapshot. */
typedef struct {
    PyObject_HEAD const plsm_message_view_t *view;
} Snapshot;
typedef struct {
    PyObject_HEAD Snapshot *snapshot;
    plsm_value_ref_t value;
    Py_ssize_t start, step, length;
    int sliced;
} Value;
typedef struct {
    PyObject_HEAD Snapshot *snapshot;
    plsm_view_cursor_t *cursor;
    plsm_value_ref_t value;
    plsm_view_entry_t entries[16];
    size_t position, count;
    Py_ssize_t index, start, step, length;
    int mode, sliced;
} Iterator;
static PyTypeObject *snapshot_type, *object_type, *array_type, *bytes_type, *iterator_type;
static PyObject *error_factory, *library_owner;
static uintptr_t configured_handle;
static PyInterpreterState *configured_interpreter;
static int (*get_view)(plsm_pool_t *, uint64_t, const plsm_message_view_t **, plsm_error_t **);
static int (*from_bytes)(const uint8_t *, size_t, uint64_t, uint64_t, const plsm_message_view_t **,
                         plsm_error_t **);
static void (*release_view)(const plsm_message_view_t *);
static uint64_t (*view_seq)(const plsm_message_view_t *), (*view_time)(const plsm_message_view_t *);
static plsm_value_ref_t (*view_root)(const plsm_message_view_t *),
    (*view_data)(const plsm_message_view_t *);
static const uint8_t *(*view_bytes)(const plsm_message_view_t *, size_t *);
static int (*value_get)(plsm_value_ref_t, const uint8_t *, size_t, plsm_value_ref_t *,
                        plsm_error_t **);
static int (*value_index)(plsm_value_ref_t, uint32_t, plsm_value_ref_t *, plsm_error_t **);
static int (*value_read)(plsm_value_ref_t, plsm_view_value_t *, plsm_error_t **);
static int (*value_iter)(plsm_value_ref_t, plsm_view_cursor_t **, plsm_error_t **);
static int (*cursor_next)(plsm_view_cursor_t *, plsm_view_entry_t *, size_t, size_t *,
                          plsm_error_t **);
static void (*cursor_free)(plsm_view_cursor_t *);
static int (*value_json)(plsm_value_ref_t, plsm_buf_t *, plsm_error_t **);
static int (*view_json)(const plsm_message_view_t *, plsm_buf_t *, plsm_error_t **);
static void (*buf_free)(plsm_buf_t *);
static int (*builder_new)(const plsm_text_t *, size_t, plsm_builder_t **, size_t *,
                          plsm_error_t **);
static int (*builder_put)(plsm_builder_t *, size_t, const uint8_t *, size_t,
                          const plsm_view_value_t *, size_t *, plsm_error_t **);
static int (*append_builder)(plsm_pool_t *, plsm_builder_t *, uint32_t,
                             const plsm_message_view_t **, plsm_error_t **);
static void (*builder_free)(plsm_builder_t *);
static int (*copy_value)(plsm_builder_t *, size_t, const uint8_t *, size_t, plsm_value_ref_t,
                         plsm_error_t **);
static int (*append_owner)(plsm_pool_t *, const plsm_message_view_t *, uint32_t,
                           const plsm_message_view_t **, plsm_error_t **);

static int (*stream_next)(plsm_stream_t *, uint64_t, size_t, size_t, const plsm_message_view_t **,
                          size_t *, plsm_error_t **);

static int fail(plsm_error_t *error) {
    PyObject *exc = PyObject_CallFunction(error_factory, "K", (unsigned long long)(uintptr_t)error);
    if (exc) {
        PyErr_SetObject((PyObject *)Py_TYPE(exc), exc);
        Py_DECREF(exc);
    }
    return -1;
}
static void drop_object(PyObject *self) {
    PyTypeObject *type = Py_TYPE(self);
    PyObject_Free(self);
    Py_DECREF(type);
}
static PyObject *wrap(Snapshot *, plsm_value_ref_t);
static PyObject *scalar_native(const plsm_view_value_t *info) {
    switch (info->kind) {
    case PLSM_FIELD_NULL:
        Py_RETURN_NONE;
    case PLSM_FIELD_BOOL:
        return PyBool_FromLong(info->integer != 0);
    case PLSM_FIELD_I64:
        return PyLong_FromLongLong(info->integer);
    case PLSM_FIELD_F64:
        return PyFloat_FromDouble(info->real);
    case PLSM_FIELD_STRING:
        return PyUnicode_DecodeUTF8((const char *)info->bytes, (Py_ssize_t)info->len, "strict");
    case PLSM_FIELD_BYTES:
        return PyBytes_FromStringAndSize((const char *)info->bytes, (Py_ssize_t)info->len);
    default:
        PyErr_SetString(PyExc_TypeError, "missing value has no native representation");
        return NULL;
    }
}
static PyObject *native(plsm_value_ref_t value) {
    plsm_view_value_t info;
    plsm_error_t *error = NULL;
    if (value_read(value, &info, &error)) {
        fail(error);
        return NULL;
    }
    if (info.kind != PLSM_FIELD_OBJECT && info.kind != PLSM_FIELD_ARRAY)
        return scalar_native(&info);
    PyObject *result =
        info.kind == PLSM_FIELD_OBJECT ? PyDict_New() : PyList_New((Py_ssize_t)info.len);
    if (!result)
        return NULL;
    plsm_view_cursor_t *cursor = NULL;
    if (value_iter(value, &cursor, &error)) {
        Py_DECREF(result);
        fail(error);
        return NULL;
    }
    Py_ssize_t index = 0;
    for (;;) {
        plsm_view_entry_t entries[16];
        size_t count = 0;
        if (cursor_next(cursor, entries, 16, &count, &error)) {
            fail(error);
            goto bad;
        }
        if (!count)
            break;
        if (PyErr_CheckSignals())
            goto bad;
        for (size_t i = 0; i < count; ++i) {
            PyObject *child = native(entries[i].value);
            if (!child)
                goto bad;
            if (info.kind == PLSM_FIELD_OBJECT) {
                PyObject *key = PyUnicode_DecodeUTF8((const char *)entries[i].key,
                                                     (Py_ssize_t)entries[i].key_len, "strict");
                int rc = key ? PyDict_SetItem(result, key, child) : -1;
                Py_XDECREF(key);
                Py_DECREF(child);
                if (rc)
                    goto bad;
            } else if (PyList_SetItem(result, index++, child))
                goto bad;
        }
    }
    cursor_free(cursor);
    return result;
bad:
    cursor_free(cursor);
    Py_DECREF(result);
    return NULL;
}
static void snapshot_dealloc(Snapshot *self) {
    if (self->view)
        release_view(self->view);
    drop_object((PyObject *)self);
}
static void value_dealloc(Value *self) {
    Py_XDECREF((PyObject *)self->snapshot);
    drop_object((PyObject *)self);
}
static void iterator_dealloc(Iterator *self) {
    if (self->cursor)
        cursor_free(self->cursor);
    Py_XDECREF((PyObject *)self->snapshot);
    drop_object((PyObject *)self);
}
static PyObject *snapshot_seq(Snapshot *self, void *closure) {
    (void)closure;
    return PyLong_FromUnsignedLongLong(view_seq(self->view));
}
static PyObject *snapshot_time(Snapshot *self, void *closure) {
    (void)closure;
    return PyLong_FromUnsignedLongLong(view_time(self->view));
}
static PyObject *snapshot_data(Snapshot *self, void *closure) {
    (void)closure;
    return wrap(self, view_data(self->view));
}
static PyObject *snapshot_root(Snapshot *self, void *closure) {
    (void)closure;
    return wrap(self, view_root(self->view));
}
static int snapshot_buffer(Snapshot *self, Py_buffer *buffer, int flags) {
    size_t length = 0;
    const uint8_t *bytes = view_bytes(self->view, &length);
    return PyBuffer_FillInfo(buffer, (PyObject *)self, (void *)bytes, (Py_ssize_t)length, 1, flags);
}
static int bytes_buffer(Value *self, Py_buffer *buffer, int flags) {
    plsm_view_value_t info;
    plsm_error_t *error = NULL;
    if (value_read(self->value, &info, &error))
        return fail(error);
    return PyBuffer_FillInfo(buffer, (PyObject *)self, (void *)info.bytes, (Py_ssize_t)info.len, 1,
                             flags);
}
static PyObject *wrap(Snapshot *owner, plsm_value_ref_t ref) {
    plsm_view_value_t info;
    plsm_error_t *error = NULL;
    if (value_read(ref, &info, &error)) {
        fail(error);
        return NULL;
    }
    if (info.kind != PLSM_FIELD_OBJECT && info.kind != PLSM_FIELD_ARRAY &&
        info.kind != PLSM_FIELD_BYTES)
        return scalar_native(&info);
    PyTypeObject *type = info.kind == PLSM_FIELD_OBJECT  ? object_type
                         : info.kind == PLSM_FIELD_ARRAY ? array_type
                                                         : bytes_type;
    Value *value = (Value *)PyType_GenericAlloc(type, 0);
    if (!value)
        return NULL;
    Py_INCREF((PyObject *)owner);
    value->snapshot = owner;
    value->value = ref;
    value->start = 0;
    value->step = 1;
    value->length = (Py_ssize_t)info.len;
    if (info.kind == PLSM_FIELD_BYTES) {
        PyObject *buffer = PyMemoryView_FromObject((PyObject *)value);
        Py_DECREF(value);
        return buffer;
    }
    return (PyObject *)value;
}
static Py_ssize_t value_length(Value *self) { return self->length; }
static PyObject *object_getitem(Value *self, PyObject *key) {
    if (!PyUnicode_Check(key)) {
        if (PyObject_Hash(key) == -1 && PyErr_Occurred())
            return NULL;
        PyErr_SetObject(PyExc_KeyError, key);
        return NULL;
    }
    Py_ssize_t length;
    const char *text = PyUnicode_AsUTF8AndSize(key, &length);
    if (!text) {
        if (PyErr_ExceptionMatches(PyExc_UnicodeEncodeError)) {
            PyErr_Clear();
            PyErr_SetObject(PyExc_KeyError, key);
        }
        return NULL;
    }
    plsm_value_ref_t child;
    plsm_error_t *error = NULL;
    if (value_get(self->value, (const uint8_t *)text, (size_t)length, &child, &error)) {
        fail(error);
        return NULL;
    }
    if (!child.owner) {
        PyErr_SetObject(PyExc_KeyError, key);
        return NULL;
    }
    return wrap(self->snapshot, child);
}
static int object_contains(Value *self, PyObject *key) {
    if (!PyUnicode_Check(key))
        return PyObject_Hash(key) == -1 && PyErr_Occurred() ? -1 : 0;
    Py_ssize_t length;
    const char *text = PyUnicode_AsUTF8AndSize(key, &length);
    if (!text) {
        if (PyErr_ExceptionMatches(PyExc_UnicodeEncodeError)) {
            PyErr_Clear();
            return 0;
        }
        return -1;
    }
    plsm_value_ref_t child;
    plsm_error_t *error = NULL;
    if (value_get(self->value, (const uint8_t *)text, (size_t)length, &child, &error))
        return fail(error);
    return child.owner != NULL;
}
static PyObject *array_getitem(Value *self, PyObject *key) {
    if (PySlice_Check(key)) {
        Py_ssize_t start, stop, step, length;
        if (PySlice_GetIndicesEx(key, self->length, &start, &stop, &step, &length))
            return NULL;
        Value *slice = (Value *)PyType_GenericAlloc(array_type, 0);
        if (!slice)
            return NULL;
        Py_INCREF((PyObject *)self->snapshot);
        slice->snapshot = self->snapshot;
        slice->value = self->value;
        slice->start = length ? self->start + start * self->step : 0;
        slice->step = length > 1 ? self->step * step : 1;
        slice->length = length;
        slice->sliced = 1;
        return (PyObject *)slice;
    }
    PyObject *integer = PyNumber_Index(key);
    if (!integer)
        return NULL;
    Py_ssize_t index = PyLong_AsSsize_t(integer);
    Py_DECREF(integer);
    if (index == -1 && PyErr_Occurred()) {
        if (PyErr_ExceptionMatches(PyExc_OverflowError)) {
            PyErr_Clear();
            PyErr_SetString(PyExc_IndexError, "array index out of range");
        }
        return NULL;
    }
    if (index < 0)
        index += self->length;
    if (index < 0 || index >= self->length) {
        PyErr_SetString(PyExc_IndexError, "array index out of range");
        return NULL;
    }
    plsm_value_ref_t child;
    plsm_error_t *error = NULL;
    if (value_index(self->value, (uint32_t)(self->start + index * self->step), &child, &error)) {
        fail(error);
        return NULL;
    }
    return wrap(self->snapshot, child);
}
static PyObject *iterator_new(Value *value, int mode) {
    Iterator *self = (Iterator *)PyType_GenericAlloc(iterator_type, 0);
    if (!self)
        return NULL;
    Py_INCREF((PyObject *)value->snapshot);
    self->snapshot = value->snapshot;
    self->value = value->value;
    self->mode = mode;
    self->start = value->start;
    self->step = value->step;
    self->length = value->length;
    self->sliced = mode == 0 && (value->start != 0 || value->step != 1);
    if (!self->sliced) {
        plsm_error_t *error = NULL;
        if (value_iter(value->value, &self->cursor, &error)) {
            Py_DECREF(self);
            fail(error);
            return NULL;
        }
    }
    return (PyObject *)self;
}
static PyObject *object_iter(Value *self) { return iterator_new(self, 1); }
static PyObject *array_iter(Value *self) { return iterator_new(self, 0); }
static PyObject *items_iter(Value *self, PyObject *ignored) {
    (void)ignored;
    return iterator_new(self, 2);
}
static PyObject *values_iter(Value *self, PyObject *ignored) {
    (void)ignored;
    return iterator_new(self, 3);
}
static PyObject *iterator_next(Iterator *self) {
    if (self->index >= self->length)
        return NULL;
    plsm_view_entry_t entry;
    plsm_error_t *error = NULL;
    if (self->sliced) {
        if (value_index(self->value, (uint32_t)(self->start + self->index * self->step),
                        &entry.value, &error)) {
            fail(error);
            return NULL;
        }
    } else {
        if (self->position == self->count) {
            if (cursor_next(self->cursor, self->entries, 16, &self->count, &error)) {
                fail(error);
                return NULL;
            }
            self->position = 0;
            if (!self->count)
                return NULL;
        }
        entry = self->entries[self->position++];
    }
    self->index++;
    if (self->mode == 0 || self->mode == 3)
        return wrap(self->snapshot, entry.value);
    PyObject *key =
        PyUnicode_DecodeUTF8((const char *)entry.key, (Py_ssize_t)entry.key_len, "strict");
    if (!key || self->mode == 1)
        return key;
    PyObject *value = wrap(self->snapshot, entry.value);
    PyObject *pair = value ? PyTuple_Pack(2, key, value) : NULL;
    Py_DECREF(key);
    Py_XDECREF(value);
    return pair;
}
static PyObject *value_native(Value *self, PyObject *ignored) {
    (void)ignored;
    if (!self->sliced)
        return native(self->value);
    PyObject *out = PyList_New(self->length);
    if (!out)
        return NULL;
    for (Py_ssize_t i = 0; i < self->length; ++i) {
        plsm_value_ref_t child;
        plsm_error_t *error = NULL;
        if (value_index(self->value, (uint32_t)(self->start + i * self->step), &child, &error)) {
            fail(error);
            Py_DECREF(out);
            return NULL;
        }
        PyObject *item = native(child);
        if (!item || PyList_SetItem(out, i, item)) {
            Py_DECREF(out);
            return NULL;
        }
    }
    return out;
}
static PyObject *value_to_json(Value *self, PyObject *ignored) {
    (void)ignored;
    plsm_buf_t buf = {0};
    plsm_error_t *error = NULL;
    if (self->sliced) {
        PyErr_SetString(PyExc_TypeError, "format sliced arrays through to_json");
        return NULL;
    }
    int rc;
    Py_BEGIN_ALLOW_THREADS
    rc = value_json(self->value, &buf, &error);
    Py_END_ALLOW_THREADS
    if (rc) {
        fail(error);
        return NULL;
    }
    PyObject *result = PyUnicode_DecodeUTF8((char *)buf.data, (Py_ssize_t)buf.len, "strict");
    buf_free(&buf);
    return result;
}
static PyObject *value_repr(Value *self) {
    return PyUnicode_FromFormat(
        "<plasmite.%s len=%zd>",
        Py_TYPE((PyObject *)self) == object_type ? "ObjectView" : "ArrayView", self->length);
}
static PyObject *snapshot_json(Snapshot *self, PyObject *ignored) {
    (void)ignored;
    plsm_buf_t buf = {0};
    plsm_error_t *error = NULL;
    int rc;
    Py_BEGIN_ALLOW_THREADS
    rc = view_json(self->view, &buf, &error);
    Py_END_ALLOW_THREADS
    if (rc) {
        fail(error);
        return NULL;
    }
    PyObject *result = PyUnicode_DecodeUTF8((char *)buf.data, (Py_ssize_t)buf.len, "strict");
    buf_free(&buf);
    return result;
}
static PyMethodDef snapshot_methods[] = {{"_json", (PyCFunction)snapshot_json, METH_NOARGS, NULL},
                                         {NULL}};
static PyObject *snapshot_repr(Snapshot *self) {
    return PyUnicode_FromFormat("<plasmite.MessageView seq=%llu>",
                                (unsigned long long)view_seq(self->view));
}
static PyGetSetDef snapshot_getset[] = {{"seq", (getter)snapshot_seq, NULL, NULL, NULL},
                                        {"timestamp_ns", (getter)snapshot_time, NULL, NULL, NULL},
                                        {"data", (getter)snapshot_data, NULL, NULL, NULL},
                                        {"_root", (getter)snapshot_root, NULL, NULL, NULL},
                                        {NULL}};
static PyMethodDef value_methods[] = {{"_native", (PyCFunction)value_native, METH_NOARGS, NULL},
                                      {"_json", (PyCFunction)value_to_json, METH_NOARGS, NULL},
                                      {"_items", (PyCFunction)items_iter, METH_NOARGS, NULL},
                                      {"_values", (PyCFunction)values_iter, METH_NOARGS, NULL},
                                      {NULL}};
static PyObject *forbid_new(PyTypeObject *type, PyObject *args, PyObject *kw) {
    (void)type;
    (void)args;
    (void)kw;
    PyErr_SetString(PyExc_TypeError, "views come from a pool or encoded snapshot");
    return NULL;
}
static PyType_Slot snapshot_slots[] = {{Py_tp_dealloc, snapshot_dealloc},
                                       {Py_tp_new, forbid_new},
                                       {Py_tp_methods, snapshot_methods},
                                       {Py_tp_getset, snapshot_getset},
                                       {Py_tp_repr, snapshot_repr},
                                       {Py_bf_getbuffer, snapshot_buffer},
                                       {0, NULL}};
static PyType_Slot object_slots[] = {{Py_tp_dealloc, value_dealloc},
                                     {Py_tp_new, forbid_new},
                                     {Py_mp_length, value_length},
                                     {Py_mp_subscript, object_getitem},
                                     {Py_sq_contains, object_contains},
                                     {Py_tp_iter, object_iter},
                                     {Py_tp_methods, value_methods},
                                     {Py_tp_repr, value_repr},
                                     {0, NULL}};
static PyType_Slot array_slots[] = {
    {Py_tp_dealloc, value_dealloc}, {Py_tp_new, forbid_new},          {Py_mp_length, value_length},
    {Py_sq_length, value_length},   {Py_mp_subscript, array_getitem}, {Py_tp_iter, array_iter},
    {Py_tp_methods, value_methods}, {Py_tp_repr, value_repr},         {0, NULL}};
static PyType_Slot bytes_slots[] = {{Py_tp_new, forbid_new},
                                    {Py_tp_dealloc, value_dealloc},
                                    {Py_bf_getbuffer, bytes_buffer},
                                    {0, NULL}};
static PyType_Slot iterator_slots[] = {{Py_tp_new, forbid_new},
                                       {Py_tp_dealloc, iterator_dealloc},
                                       {Py_tp_iter, PyObject_SelfIter},
                                       {Py_tp_iternext, iterator_next},
                                       {0, NULL}};
static PyTypeObject *make_type(const char *name, int size, PyType_Slot *slots) {
    PyType_Spec spec = {name, size, 0, Py_TPFLAGS_DEFAULT, slots};
    return (PyTypeObject *)PyType_FromSpec(&spec);
}
static PyObject *snapshot_from_owner(const plsm_message_view_t *view) {
    Snapshot *result = (Snapshot *)PyType_GenericAlloc(snapshot_type, 0);
    if (!result) {
        release_view(view);
        return NULL;
    }
    result->view = view;
    return (PyObject *)result;
}

static PyObject *adopt_owner(PyObject *module, PyObject *arg) {
    (void)module;
    void *owner = PyLong_AsVoidPtr(arg);
    if (PyErr_Occurred()) return NULL;
    if (!owner) {
        PyErr_SetString(PyExc_ValueError, "null message owner");
        return NULL;
    }
    return snapshot_from_owner(owner);
}

static PyObject *next_snapshots(PyObject *module, PyObject *args) {
    (void)module;
    unsigned long long address, timeout;
    Py_ssize_t max_count, max_bytes;
    if (!PyArg_ParseTuple(args, "KnnK", &address, &max_count, &max_bytes, &timeout))
        return NULL;
    if (max_count <= 0 || max_bytes <= 0 ||
        (size_t)max_count > SIZE_MAX / sizeof(plsm_message_view_t *)) {
        PyErr_SetString(PyExc_ValueError, "batch bounds must be positive and fit address space");
        return NULL;
    }
    const plsm_message_view_t *local[16];
    const plsm_message_view_t **owners =
        max_count <= 16 ? local : PyMem_Malloc((size_t)max_count * sizeof(*owners));
    if (!owners)
        return PyErr_NoMemory();
    size_t count = 0;
    plsm_error_t *error = NULL;
    int status;
    Py_BEGIN_ALLOW_THREADS
    status = stream_next((plsm_stream_t *)(uintptr_t)address, timeout, (size_t)max_count,
                         (size_t)max_bytes, owners, &count, &error);
    Py_END_ALLOW_THREADS
    PyObject *items = status < 0 ? NULL : PyList_New((Py_ssize_t)count);
    if (status < 0)
        fail(error);
    for (size_t i = 0; i < count; ++i) {
        if (items) {
            PyObject *snapshot = snapshot_from_owner(owners[i]);
            if (!snapshot) {
                Py_CLEAR(items);
            } else
                PyList_SetItem(items, (Py_ssize_t)i, snapshot);
        } else
            release_view(owners[i]);
    }
    if (owners != local)
        PyMem_Free(owners);
    if (!items)
        return NULL;
    PyObject *result = Py_BuildValue("iO", status, items);
    Py_DECREF(items);
    return result;
}
static PyObject *get_snapshot(PyObject *module, PyObject *args) {
    (void)module;
    if (!configured_handle) {
        PyErr_SetString(PyExc_RuntimeError, "view backend is not configured");
        return NULL;
    }
    PyObject *address, *sequence;
    if (!PyArg_ParseTuple(args, "OO", &address, &sequence))
        return NULL;
    plsm_pool_t *pool = PyLong_AsVoidPtr(address);
    if (PyErr_Occurred())
        return NULL;
    uint64_t seq = PyLong_AsUnsignedLongLong(sequence);
    if (PyErr_Occurred())
        return NULL;
    const plsm_message_view_t *view = NULL;
    plsm_error_t *error = NULL;
    int rc;
    Py_BEGIN_ALLOW_THREADS
    rc = get_view(pool, seq, &view, &error);
    Py_END_ALLOW_THREADS
    if (rc) {
        fail(error);
        return NULL;
    }
    return snapshot_from_owner(view);
}
static PyObject *bytes_snapshot(PyObject *module, PyObject *args) {
    (void)module;
    if (!configured_handle) {
        PyErr_SetString(PyExc_RuntimeError, "view backend is not configured");
        return NULL;
    }
    PyObject *payload;
    char *bytes;
    Py_ssize_t length;
    unsigned long long seq, time;
    if (!PyArg_ParseTuple(args, "OKK", &payload, &seq, &time))
        return NULL;
    if (!PyBytes_Check(payload)) {
        PyErr_SetString(PyExc_TypeError, "native snapshot input must be immutable bytes");
        return NULL;
    }
    if (PyBytes_AsStringAndSize(payload, &bytes, &length))
        return NULL;
    const plsm_message_view_t *view = NULL;
    plsm_error_t *error = NULL;
    int rc;
    /* bytes_snapshot accepts immutable bytes; libplasmite copies before return. */
    Py_BEGIN_ALLOW_THREADS
    rc = from_bytes((const uint8_t *)bytes, (size_t)length, seq, time, &view, &error);
    Py_END_ALLOW_THREADS
    if (rc) {
        fail(error);
        return NULL;
    }
    return snapshot_from_owner(view);
}
/* Exact builtin input cannot invoke Python hooks while the encoder grows. */
typedef struct Ancestor {
    PyObject *value;
    const struct Ancestor *parent;
} Ancestor;

static int write_python(plsm_builder_t *, size_t, const uint8_t *, size_t, PyObject *,
                        const Ancestor *);
static int write_container(plsm_builder_t *builder, size_t node, PyObject *value,
                           const Ancestor *parents) {
    for (const Ancestor *p = parents; p; p = p->parent) {
        if (p->value == value) {
            PyErr_SetString(PyExc_ValueError, "circular Python input");
            return -1;
        }
    }
    if (Py_EnterRecursiveCall(" while encoding Plasmite input"))
        return -1;
    Ancestor current = {value, parents};
    int result = 0;
    if (PyDict_CheckExact(value)) {
        PyObject *key, *child;
        Py_ssize_t position = 0;
        while (PyDict_Next(value, &position, &key, &child)) {
            if (!PyUnicode_CheckExact(key)) {
                PyErr_SetString(PyExc_TypeError,
                                "append_view object keys must be exact str values");
                result = -1;
                break;
            }
            Py_ssize_t length;
            const char *text = PyUnicode_AsUTF8AndSize(key, &length);
            if (!text || write_python(builder, node, (const uint8_t *)text, (size_t)length, child,
                                      &current)) {
                result = -1;
                break;
            }
        }
    } else {
        int list = PyList_CheckExact(value);
        Py_ssize_t count = list ? PyList_Size(value) : PyTuple_Size(value);
        for (Py_ssize_t i = 0; i < count; ++i) {
            PyObject *child = list ? PyList_GetItem(value, i) : PyTuple_GetItem(value, i);
            if (!child || write_python(builder, node, NULL, 0, child, &current)) {
                result = -1;
                break;
            }
        }
    }
    Py_LeaveRecursiveCall();
    return result;
}
static int write_python(plsm_builder_t *builder, size_t parent, const uint8_t *key, size_t key_len,
                        PyObject *value, const Ancestor *parents) {
    if (Py_IS_TYPE(value, object_type) || Py_IS_TYPE(value, array_type)) {
        Value *source = (Value *)value;
        plsm_error_t *error = NULL;
        if (!source->sliced) {
            int rc = copy_value(builder, parent, key, key_len, source->value, &error);
            return rc ? fail(error) : 0;
        }
        plsm_view_value_t array = {0};
        array.kind = PLSM_FIELD_ARRAY;
        size_t node;
        if (builder_put(builder, parent, key, key_len, &array, &node, &error))
            return fail(error);
        for (Py_ssize_t i = 0; i < source->length; ++i) {
            plsm_value_ref_t child;
            if (value_index(source->value, (uint32_t)(source->start + i * source->step), &child,
                            &error) ||
                copy_value(builder, node, NULL, 0, child, &error))
                return fail(error);
        }
        return 0;
    }
    plsm_view_value_t input = {0};
    Py_buffer buffer = {0};
    PyObject *contiguous = NULL;
    int container = 0;
    if (value == Py_None)
        input.kind = PLSM_FIELD_NULL;
    else if (PyBool_Check(value)) {
        input.kind = PLSM_FIELD_BOOL;
        input.integer = value == Py_True;
    } else if (PyLong_CheckExact(value)) {
        input.kind = PLSM_FIELD_I64;
        input.integer = PyLong_AsLongLong(value);
        if (PyErr_Occurred())
            return -1;
    } else if (PyFloat_CheckExact(value)) {
        input.kind = PLSM_FIELD_F64;
        input.real = PyFloat_AsDouble(value);
    } else if (PyUnicode_CheckExact(value)) {
        Py_ssize_t length;
        input.kind = PLSM_FIELD_STRING;
        input.bytes = (const uint8_t *)PyUnicode_AsUTF8AndSize(value, &length);
        if (!input.bytes)
            return -1;
        input.len = (size_t)length;
    } else if (PyBytes_CheckExact(value) || PyByteArray_CheckExact(value) ||
               PyMemoryView_Check(value)) {
        if (PyObject_GetBuffer(value, &buffer, PyBUF_FULL_RO))
            return -1;
        input.kind = PLSM_FIELD_BYTES;
        input.bytes = (const uint8_t *)buffer.buf;
        input.len = (size_t)buffer.len;
        if (!PyBuffer_IsContiguous(&buffer, 'C')) {
            contiguous = PyBytes_FromStringAndSize(NULL, buffer.len);
            char *bytes;
            Py_ssize_t length;
            if (!contiguous || PyBytes_AsStringAndSize(contiguous, &bytes, &length) ||
                PyBuffer_ToContiguous(bytes, &buffer, buffer.len, 'C')) {
                Py_XDECREF(contiguous);
                PyBuffer_Release(&buffer);
                return -1;
            }
            input.bytes = (const uint8_t *)bytes;
        }
    } else if (PyDict_CheckExact(value)) {
        input.kind = PLSM_FIELD_OBJECT;
        container = 1;
    } else if (PyList_CheckExact(value) || PyTuple_CheckExact(value)) {
        input.kind = PLSM_FIELD_ARRAY;
        container = 1;
    } else {
        PyErr_SetString(
            PyExc_TypeError,
            "append_view supports exact builtin dict/list/tuple/scalars and byte buffers; "
            "use append for JSON-compatible subclasses");
        return -1;
    }
    size_t node = 0;
    plsm_error_t *error = NULL;
    int rc = builder_put(builder, parent, key, key_len, &input, &node, &error);
    if (buffer.obj)
        PyBuffer_Release(&buffer);
    Py_XDECREF(contiguous);
    if (rc)
        return fail(error);
    return container ? write_container(builder, node, value, parents) : 0;
}
static PyObject *append_python(PyObject *module, PyObject *args) {
    (void)module;
    if (!configured_handle) {
        PyErr_SetString(PyExc_RuntimeError, "view backend is not configured");
        return NULL;
    }
    PyObject *address, *data, *tags;
    unsigned int durability;
    if (!PyArg_ParseTuple(args, "OOOI", &address, &data, &tags, &durability))
        return NULL;
    plsm_pool_t *pool = PyLong_AsVoidPtr(address);
    if (PyErr_Occurred())
        return NULL;
    if (Py_IS_TYPE(data, snapshot_type) && tags == Py_None) {
        const plsm_message_view_t *view = NULL;
        plsm_error_t *error = NULL;
        int rc;
        Py_BEGIN_ALLOW_THREADS
        rc = append_owner(pool, ((Snapshot *)data)->view, durability, &view, &error);
        Py_END_ALLOW_THREADS
        if (rc) {
            fail(error);
            return NULL;
        }
        return snapshot_from_owner(view);
    }
    if (!PyDict_CheckExact(data) && !Py_IS_TYPE(data, object_type) &&
        !Py_IS_TYPE(data, snapshot_type)) {
        PyErr_SetString(PyExc_TypeError,
                        "append_view data must be an exact dict, ObjectView or MessageView");
        return NULL;
    }
    if (tags != Py_None && !PyList_CheckExact(tags)) {
        PyErr_SetString(PyExc_TypeError, "append_view tags must be a list of exact strings");
        return NULL;
    }
    Py_ssize_t count = tags == Py_None ? 0 : PyList_Size(tags);
    plsm_text_t *spans = count ? PyMem_Calloc((size_t)count, sizeof(plsm_text_t)) : NULL;
    if (count && !spans)
        return PyErr_NoMemory();
    for (Py_ssize_t i = 0; i < count; ++i) {
        PyObject *tag = PyList_GetItem(tags, i);
        if (!PyUnicode_CheckExact(tag)) {
            PyMem_Free(spans);
            PyErr_SetString(PyExc_TypeError, "append_view tags must be exact str values");
            return NULL;
        }
        Py_ssize_t length;
        spans[i].data = (const uint8_t *)PyUnicode_AsUTF8AndSize(tag, &length);
        if (!spans[i].data) {
            PyMem_Free(spans);
            return NULL;
        }
        spans[i].len = (size_t)length;
    }
    plsm_builder_t *builder = NULL;
    size_t node = 0;
    plsm_error_t *error = NULL;
    int rc = builder_new(spans, (size_t)count, &builder, &node, &error);
    PyMem_Free(spans);
    if (rc) {
        fail(error);
        return NULL;
    }
    if (PyDict_CheckExact(data)) {
        if (write_container(builder, node, data, NULL)) {
            builder_free(builder);
            return NULL;
        }
    } else {
        plsm_value_ref_t source = Py_IS_TYPE(data, snapshot_type)
                                      ? view_data(((Snapshot *)data)->view)
                                      : ((Value *)data)->value;
        if (copy_value(builder, SIZE_MAX, NULL, 0, source, &error)) {
            builder_free(builder);
            fail(error);
            return NULL;
        }
    }
    const plsm_message_view_t *view = NULL;
    /* Input access has finished; the builder now owns all bytes. */
    Py_BEGIN_ALLOW_THREADS
    rc = append_builder(pool, builder, durability, &view, &error);
    builder_free(builder);
    Py_END_ALLOW_THREADS
    if (rc) {
        fail(error);
        return NULL;
    }
    return snapshot_from_owner(view);
}

static PyObject *configure(PyObject *module, PyObject *args) {
    (void)module;
    unsigned long long address;
    PyObject *factory, *library;
    if (!PyArg_ParseTuple(args, "KOO", &address, &factory, &library))
        return NULL;
    if (configured_interpreter && configured_interpreter != PyInterpreterState_Get()) {
        PyErr_SetString(PyExc_ImportError, "native views support one interpreter per process");
        return NULL;
    }
    if (!PyCallable_Check(factory)) {
        PyErr_SetString(PyExc_TypeError, "error factory must be callable");
        return NULL;
    }
    if (configured_handle && configured_handle != (uintptr_t)address) {
        PyErr_SetString(PyExc_ImportError, "native views keep one libplasmite per process");
        return NULL;
    }
#ifdef _WIN32
    HMODULE handle = (HMODULE)(uintptr_t)address;
#define SYMBOL(name) ((void *)GetProcAddress(handle, name))
#else
    void *handle = (void *)(uintptr_t)address;
#define SYMBOL(name) dlsym(handle, name)
#endif
    if (!handle) {
        PyErr_SetString(PyExc_ImportError, "cannot load libplasmite for native views");
        return NULL;
    }
#define LOAD(field, name)                                                                          \
    do {                                                                                           \
        void *symbol = SYMBOL(name);                                                               \
        if (!symbol) {                                                                             \
            PyErr_Format(PyExc_ImportError, "libplasmite lacks %s", name);                         \
            return NULL;                                                                           \
        }                                                                                          \
        memcpy(&field, &symbol, sizeof(field));                                                    \
    } while (0)
    LOAD(get_view, "plsm_pool_get_view");
    LOAD(from_bytes, "plsm_view_from_lite3");
    LOAD(release_view, "plsm_view_release");
    LOAD(view_seq, "plsm_view_seq");
    LOAD(view_time, "plsm_view_timestamp_ns");
    LOAD(view_root, "plsm_view_root");
    LOAD(view_data, "plsm_view_data");
    LOAD(view_bytes, "plsm_view_bytes");
    LOAD(value_get, "plsm_value_get");
    LOAD(value_index, "plsm_value_index");
    LOAD(value_read, "plsm_value_read");
    LOAD(value_iter, "plsm_value_iter");
    LOAD(cursor_next, "plsm_cursor_next");
    LOAD(cursor_free, "plsm_cursor_free");
    LOAD(value_json, "plsm_value_to_json");
    LOAD(view_json, "plsm_view_to_json");
    LOAD(buf_free, "plsm_buf_free");
    LOAD(builder_new, "plsm_builder_new");
    LOAD(builder_put, "plsm_builder_put");
    LOAD(append_builder, "plsm_pool_append_builder");
    LOAD(builder_free, "plsm_builder_free");
    LOAD(copy_value, "plsm_builder_copy_value");
    LOAD(append_owner, "plsm_pool_append_view");
    LOAD(stream_next, "plsm_view_stream_next");
    configured_interpreter = PyInterpreterState_Get();
    configured_handle = (uintptr_t)address;
    Py_INCREF(library);
    Py_XDECREF(library_owner);
    library_owner = library;
    Py_INCREF(factory);
    Py_XDECREF(error_factory);
    error_factory = factory;
    Py_RETURN_NONE;
}
static PyMethodDef module_methods[] = {{"_configure", configure, METH_VARARGS, NULL},
                                       {"adopt_owner", adopt_owner, METH_O, NULL},
                                       {"get", get_snapshot, METH_VARARGS, NULL},
                                       {"append", append_python, METH_VARARGS, NULL},
                                       {"from_bytes", bytes_snapshot, METH_VARARGS, NULL},
                                       {"next_batch", next_snapshots, METH_VARARGS, NULL},
                                       {NULL}};
static struct PyModuleDef definition = {PyModuleDef_HEAD_INIT, "_views_native", NULL, -1,
                                        module_methods};
PyMODINIT_FUNC PyInit__views_native(void) {
    if (configured_interpreter && configured_interpreter != PyInterpreterState_Get()) {
        PyErr_SetString(PyExc_ImportError, "native views support one interpreter per process");
        return NULL;
    }
    PyObject *module = PyModule_Create(&definition);
    if (!module)
        return NULL;
    snapshot_type = make_type("plasmite.MessageView", sizeof(Snapshot), snapshot_slots);
    object_type = make_type("plasmite.ObjectView", sizeof(Value), object_slots);
    array_type = make_type("plasmite.ArrayView", sizeof(Value), array_slots);
    bytes_type = make_type("plasmite._BytesView", sizeof(Value), bytes_slots);
    iterator_type = make_type("plasmite._ViewIterator", sizeof(Iterator), iterator_slots);
    if (!snapshot_type || !object_type || !array_type || !bytes_type || !iterator_type) {
        Py_DECREF(module);
        return NULL;
    }
    PyModule_AddObject(module, "MessageView", (PyObject *)snapshot_type);
    PyModule_AddObject(module, "ObjectView", (PyObject *)object_type);
    PyModule_AddObject(module, "ArrayView", (PyObject *)array_type);
    /* Keep internal factory type references for the module lifetime. */
    PyModule_AddObject(module, "_BytesView", (PyObject *)bytes_type);
    PyModule_AddObject(module, "_ViewIterator", (PyObject *)iterator_type);
    return module;
}
