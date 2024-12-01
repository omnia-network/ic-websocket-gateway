#!/bin/bash

set -e

./scripts/unit_test.sh
./scripts/prepare_tests.sh
./scripts/integration_test_rs.sh
./scripts/integration_test_mo.sh
