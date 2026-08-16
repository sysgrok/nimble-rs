/*
 * The bindgen surface of `nimble-rs-sys`.
 *
 * Note that the *binding* surface is not the *link* surface: this header pulls
 * in every public esp-nimble header the safe `nimble-rs` crate (or an advanced
 * user) may need, regardless of which parts of the stack the active feature
 * set actually compiles. Bindings to a function that is not compiled are
 * harmless as long as the function is not called.
 */

#include "esp_err.h"

#include "syscfg/syscfg.h"

#include "os/os.h"
#include "os/os_mbuf.h"
#include "os/os_mempool.h"

#include "nimble/ble.h"
#include "nimble/hci_common.h"
#include "nimble/nimble_npl.h"
#include "nimble/nimble_port.h"
#include "nimble/transport.h"
#include "nimble/transport_impl.h"

#include "host/ble_hs.h"
#include "host/ble_hs_stop.h"
#include "host/ble_l2cap.h"
#include "host/ble_store.h"
#include "host/util/util.h"

#include "services/gap/ble_svc_gap.h"
#include "services/gatt/ble_svc_gatt.h"

#include "store/ram/ble_store_ram.h"

#include "esp_nimble_mem.h"
