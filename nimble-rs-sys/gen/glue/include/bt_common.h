/*
 * Minimal stand-in for ESP-IDF's `bt_common.h` (`components/bt/common/include`).
 *
 * Required because the esp-nimble fork includes it unconditionally from
 * `nimble/host/src/ble_hs.c` and `nimble/host/src/ble_hs_hci*.c` (verified on
 * master 274b98003). The only macro those files consume from it is
 * `BT_HCI_LOG_INCLUDED`, which gates the (ESP-only) HCI packet logger.
 */
#ifndef NIMBLE_RS_GLUE_BT_COMMON_H
#define NIMBLE_RS_GLUE_BT_COMMON_H

#ifndef TRUE
#define TRUE 1
#endif

#ifndef FALSE
#define FALSE 0
#endif

#define BT_HCI_LOG_INCLUDED FALSE

#endif /* NIMBLE_RS_GLUE_BT_COMMON_H */
