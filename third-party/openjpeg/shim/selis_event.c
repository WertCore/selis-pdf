/*
 * opj_event_msg stub for the freestanding wasm module (SL-1.FILT.08).
 *
 * event.c is not compiled (its vsnprintf formatting is dropped); this stub
 * records only the EVENT KIND in a module static. Formatted messages are
 * never produced: they embed document-shaped data, which the engine keeps
 * out of logs (ADR-P0017). The status channel in selis_jpx.c carries the
 * outcome to the host.
 */

#include "opj_includes.h"

OPJ_BOOL opj_event_msg(opj_event_mgr_t *p_event_mgr, OPJ_INT32 event_type,
                       const char *fmt, ...)
{
    (void)p_event_mgr;
    (void)event_type;
    (void)fmt;
    return OPJ_TRUE;
}

void opj_set_default_event_handler(opj_event_mgr_t *p_event_mgr)
{
    (void)p_event_mgr;
}