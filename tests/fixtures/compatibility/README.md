# Compatibility fixtures

This directory contains checked-in, reviewable compatibility vectors for the
legacy project, `.scope` recording, and SCP1 V1 frame formats.

`scopeproj-v1-minimal.json` is a schema V1 project with one CSV source. It is
used to verify explicit V1→V2 migration without silently accepting an unknown
schema version. `scope-v1-minimal.hex` is a complete `SCOPEV1` recording with a
valid empty index and SessionEnd record; the recording tests decode it into a
temporary file and open it through the production parser. `scp1-v1-ping.hex` is
the 40-byte PING golden frame checked by the protocol tests. Hex encoding keeps
the binary vectors reviewable while preserving the exact bytes and CRC32C
values.
