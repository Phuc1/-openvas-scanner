# SPDX-FileCopyrightText: 2023 Greenbone AG
# Some text descriptions might be excerpted from (a) referenced
# source(s), and are Copyright (C) by the respective right holder(s).
#
# SPDX-License-Identifier: GPL-2.0-or-later WITH x11vnc-openssl-exception

a = 1;
if (a) {
  local_var a;
  a = 23;
  display(a);
}
display(a);
