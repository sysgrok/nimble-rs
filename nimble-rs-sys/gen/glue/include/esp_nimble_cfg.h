/*
 * Empty stand-in for ESP-IDF's `esp_nimble_cfg.h`
 * (`components/bt/host/nimble/port/include`), the Kconfig -> syscfg mapping
 * header, which `services/gap/ble_svc_gap.h` includes unconditionally.
 *
 * nimble-rs does not use ESP-IDF's Kconfig: the entire NimBLE configuration is
 * passed as explicit `-DMYNEWT_VAL_*` compiler flags derived from Cargo
 * features (see `gen/features.rs`), so there is nothing to map here.
 */
#ifndef NIMBLE_RS_GLUE_ESP_NIMBLE_CFG_H
#define NIMBLE_RS_GLUE_ESP_NIMBLE_CFG_H

#endif /* NIMBLE_RS_GLUE_ESP_NIMBLE_CFG_H */
