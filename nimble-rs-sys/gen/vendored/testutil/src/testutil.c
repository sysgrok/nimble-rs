/*
 * Licensed to the Apache Software Foundation (ASF) under one
 * or more contributor license agreements.  See the NOTICE file
 * distributed with this work for additional information
 * regarding copyright ownership.  The ASF licenses this file
 * to you under the Apache License, Version 2.0 (the
 * "License"); you may not use this file except in compliance
 * with the License.  You may obtain a copy of the License at
 * 
 *  http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing,
 * software distributed under the License is distributed on an
 * "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
 * KIND, either express or implied.  See the License for the
 * specific language governing permissions and limitations
 * under the License.
 */

#include <assert.h>
#include <errno.h>
#include <unistd.h>
/* nimble-rs: vendored from apache/mynewt-core @ mynewt_1_13_0_tag
 * (test/testutil), patched for the SELFTEST-only build against the esp-nimble
 * porting layer: the mynewt kernel/hal includes are replaced with the porting
 * shims; the mynewt-task section at the end is removed (SELFTEST runs caseless-task); os_init/os_arch_os_stop are no-ops. */
#include <stdio.h>
#include "syscfg/syscfg.h"
#include "sysinit/sysinit.h"

#define os_init(fn)
#define os_arch_os_stop()
#include "testutil/testutil.h"
#include "testutil_priv.h"

/* The test task runs at a lower priority (greater number) than the default
 * task.  This allows the test task to assume events get processed as soon as
 * they are initiated.  The test code can then immediately assert the expected
 * result of event processing.
 */
#define TU_TEST_TASK_PRIO   (MYNEWT_VAL(OS_MAIN_TASK_PRIO) + 1)
#define TU_TEST_STACK_SIZE  1024

struct tu_config tu_config;

int tu_any_failed;

struct ts_testsuite_list *ts_suites;

void
tu_set_pass_cb(tu_case_report_fn_t *cb, void *cb_arg)
{
    tu_config.pass_cb = cb;
    tu_config.pass_arg = cb_arg;
}

void
tu_set_fail_cb(tu_case_report_fn_t *cb, void *cb_arg)
{
    tu_config.fail_cb = cb;
    tu_config.fail_arg = cb_arg;
}

#if MYNEWT_VAL(SELFTEST)
static void
tu_pass_cb_self(const char *msg, void *arg)
{
    printf("[pass] %s/%s %s\n", tu_config.ts_suite_name, tu_case_name, msg);
    fflush(stdout);
}

static void
tu_fail_cb_self(const char *msg, void *arg)
{
    printf("[FAIL] %s/%s %s\n", tu_config.ts_suite_name, tu_case_name, msg);
    fflush(stdout);
}
#endif

void
tu_init(void)
{
    /* Ensure this function only gets called by sysinit. */
    SYSINIT_ASSERT_ACTIVE();

#if MYNEWT_VAL(SELFTEST)
    os_init(NULL);
    tu_set_pass_cb(tu_pass_cb_self, NULL);
    tu_set_fail_cb(tu_fail_cb_self, NULL);
#endif
}

void
tu_arch_restart(void)
{
#if MYNEWT_VAL(SELFTEST)
    os_arch_os_stop();
    tu_case_abort();
#else
    hal_system_reset();
#endif
}

void
tu_restart(void)
{
    tu_case_write_pass_auto();
    tu_arch_restart();
}

/* (mynewt task-based test runner removed - SELFTEST only) */
