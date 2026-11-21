"""Thin benchmark-only ctypes adapter for the additive owned-view ABI.

This measures the C boundary, not the proposed compiled Python extension.
"""
import ctypes as c
import json


class ViewABI:
    def __init__(self, module):
        self.module = module
        self.lib = module._LIB
        error = c.POINTER(c.POINTER(module.plsm_error_t))
        owner = c.POINTER(c.c_void_p)
        pool = c.POINTER(module.plsm_pool_t)
        self.lib.plsm_internal_pool_feed_json_view.argtypes = [pool, c.c_char_p, c.c_size_t,
                                                        c.POINTER(module.plsm_feed_options_t), owner, error]
        self.lib.plsm_internal_pool_feed_json_view.restype = c.c_int
        self.lib.plsm_view_from_lite3.argtypes = [c.c_char_p, c.c_size_t,
            c.c_uint64, c.c_uint64, owner, error]
        self.lib.plsm_view_from_lite3.restype = c.c_int
        self.lib.plsm_internal_pool_feed_view.argtypes = [pool, c.c_void_p, c.c_void_p, owner, error]
        self.lib.plsm_internal_pool_feed_view.restype = c.c_int
        for name in ['plsm_view_seq', 'plsm_view_time']:
            function = getattr(self.lib, name)
            function.argtypes = [c.c_void_p]
            function.restype = c.c_uint64
        self.lib.plsm_view_release.argtypes = [c.c_void_p]
        self.lib.plsm_view_release.restype = None

    def checked(self, function, *arguments):
        output = c.c_void_p()
        error = c.POINTER(self.module.plsm_error_t)()
        if function(*arguments, c.byref(output), c.byref(error)) != 0:
            raise self.module._take_error(error)
        return output

    def from_bytes(self, payload):
        return self.checked(self.lib.plsm_view_from_lite3, payload, len(payload), 1, 1)

    def receipt(self, owner):
        try:
            return self.module.FeedReceipt(self.lib.plsm_view_seq(owner),
                                             self.lib.plsm_view_time(owner))
        finally:
            self.lib.plsm_view_release(owner)

    def append(self, pool, value):
        payload = json.dumps(value).encode('utf-8')
        return self.receipt(self.checked(self.lib.plsm_internal_pool_feed_json_view,
                            pool._ptr, payload, len(payload), None))

    def forward(self, pool, owner):
        return self.receipt(self.checked(self.lib.plsm_internal_pool_feed_view,
                                        pool._ptr, owner, 0))
