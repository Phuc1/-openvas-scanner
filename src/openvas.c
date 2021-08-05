/* Portions Copyright (C) 2009-2021 Greenbone Networks GmbH
 * Portions Copyright (C) 2006 Software in the Public Interest, Inc.
 * Based on work Copyright (C) 1998 - 2006 Tenable Network Security, Inc.
 *
 * SPDX-License-Identifier: GPL-2.0-only
 *
 * This program is free software; you can redistribute it and/or
 * modify it under the terms of the GNU General Public License
 * version 2 as published by the Free Software Foundation.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program; if not, write to the Free Software
 * Foundation, Inc., 51 Franklin St, Fifth Floor, Boston, MA 02110-1301 USA.
 */

/**
 * @mainpage
 *
 * @section Introduction
 * @verbinclude README.md
 *
 * @section license License Information
 * @verbinclude COPYING
 */

/**
 * @file
 * OpenVAS main module, runs the scanner.
 */

#include "../misc/plugutils.h"     /* nvticache_free */
#include "../misc/vendorversion.h" /* for vendor_version_set */
#include "attack.h"                /* for attack_network */
#include "debug_utils.h"           /* for init_sentry */
#include "pluginlaunch.h"          /* for init_loading_shm */
#include "processes.h"             /* for create_process */
#include "sighand.h"               /* for openvas_signal */
#include "utils.h"                 /* for store_file */

#include <errno.h>  /* for errno() */
#include <fcntl.h>  /* for open() */
#include <gcrypt.h> /* for gcry_control */
#include <glib.h>
#include <gnutls/gnutls.h> /* for gnutls_global_set_log_*  */
#include <grp.h>
#include <gvm/base/logging.h> /* for setup_log_handler, load_log_configuration, free_log_configuration*/
#include <gvm/base/nvti.h>      /* for prefs_get() */
#include <gvm/base/prefs.h>     /* for prefs_get() */
#include <gvm/base/proctitle.h> /* for proctitle_set */
#include <gvm/base/version.h>   /* for gvm_libs_version */
#include <gvm/util/kb.h>        /* for KB_PATH_DEFAULT */
#include <gvm/util/mqtt.h>      /* for mqtt_init */
#include <gvm/util/nvticache.h> /* nvticache_free */
#include <gvm/util/uuidutils.h> /* gvm_uuid_make */
#include <json-glib/json-glib.h>
#include <netdb.h> /* for addrinfo */
#include <pwd.h>
#include <signal.h> /* for SIGTERM */
#include <stdio.h>  /* for fflush() */
#include <stdlib.h> /* for atoi() */
#include <sys/stat.h>
#include <sys/un.h>
#include <sys/wait.h> /* for waitpid */
#include <time.h>
#include <unistd.h> /* for close() */

#ifdef GIT_REV_AVAILABLE
#include "gitrevision.h"
#endif

#if GNUTLS_VERSION_NUMBER < 0x030300
#include "../misc/network.h" /* openvas_SSL_init */
#endif

#undef G_LOG_DOMAIN
/**
 * @brief GLib log domain.
 */
#define G_LOG_DOMAIN "sd   main"

#define PROCTITLE_WAITING "openvas: Waiting for incoming connections"
#define PROCTITLE_LOADING "openvas: Loading Handler"
#define PROCTITLE_RELOADING "openvas: Reloading"
#define PROCTITLE_SERVING "openvas: Serving %s"

/**
 * Globals that should not be touched (used in utils module).
 */
int global_max_hosts = 15;
int global_max_checks = 10;

int global_min_memory = 0;
int global_max_sysload = 0;

/**
 * @brief Logging parameters, as passed to setup_log_handlers.
 */
GSList *log_config = NULL;

static volatile int termination_signal = 0;
static char *global_scan_id = NULL;

typedef struct
{
  char *option;
  char *value;
} openvas_option;

/**
 * @brief Default values for scanner options. Must be NULL terminated.
 *
 * Only include options which are dependent on CMake variables.
 * Empty options must be "\0", not NULL, to match the behavior of prefs_init.
 */
static openvas_option openvas_defaults[] = {
  {"plugins_folder", OPENVAS_NVT_DIR},
  {"include_folders", OPENVAS_NVT_DIR},
  {"plugins_timeout", G_STRINGIFY (NVT_TIMEOUT)},
  {"scanner_plugins_timeout", G_STRINGIFY (SCANNER_NVT_TIMEOUT)},
  {"db_address", KB_PATH_DEFAULT},
  {NULL, NULL}};

/**
 * @brief Set the prefs from the openvas_defaults array.
 */
static void
set_default_openvas_prefs ()
{
  for (int i = 0; openvas_defaults[i].option != NULL; i++)
    prefs_set (openvas_defaults[i].option, openvas_defaults[i].value);
}

static void
my_gnutls_log_func (int level, const char *text)
{
  g_message ("(%d) %s", level, text);
}

static void
set_globals_from_preferences (void)
{
  const char *str;

  if ((str = prefs_get ("max_hosts")) != NULL)
    {
      global_max_hosts = atoi (str);
      if (global_max_hosts <= 0)
        global_max_hosts = 15;
    }

  if ((str = prefs_get ("max_checks")) != NULL)
    {
      global_max_checks = atoi (str);
      if (global_max_checks <= 0)
        global_max_checks = 10;
    }

  if ((str = prefs_get ("max_sysload")) != NULL)
    {
      global_max_sysload = atoi (str);
      if (global_max_sysload <= 0)
        global_max_sysload = 0;
    }

  if ((str = prefs_get ("min_free_mem")) != NULL)
    {
      global_min_memory = atoi (str);
      if (global_min_memory <= 0)
        global_min_memory = 0;
    }
}

static void
handle_termination_signal (int sig)
{
  termination_signal = sig;
}

/**
 * @brief Initializes main scanner process' signal handlers.
 */
static void
init_signal_handlers (void)
{
  openvas_signal (SIGTERM, handle_termination_signal);
  openvas_signal (SIGINT, handle_termination_signal);
  openvas_signal (SIGQUIT, handle_termination_signal);
  openvas_signal (SIGCHLD, sighand_chld);
}

/**
 * @brief Read preferences from json recursively
 *
 * Adds preferences from a json string to the global_prefs.
 * If preference already exists in global_prefs they will be overwritten by
 * prefs from json.
 *
 * @param globals Scan ID of globals used as key to find the corresponding KB
 * where to take the preferences from. Globals also used for file upload.
 * @param json String in which preferences are stored.
 * @return int 0 on success, -1 if json is empty or format is invalid.
 */
static int
write_json_preferences_recursive (char *json)
{
  JsonParser *parser;
  JsonReader *reader;

  gint num_member;
  gchar **members;

  int i;

  // Build json tree struct
  parser = json_parser_new ();
  if (!json_parser_load_from_data (parser, json, -1, NULL))
    {
      return -1;
    }
  reader = json_reader_new (json_parser_get_root (parser));

  num_member = json_reader_count_members (reader);
  if (num_member < 1)
    {
      return -1;
    }

  members = json_reader_list_members (reader);

  for (i = 0; i < num_member; i++)
    {
      gchar *key = members[i];
      g_message ("PROCESSING %s", key);
      if (!strcmp (key, "created") || !strcmp (key, "message_type")
          || !strcmp (key, "group_id") || !strcmp (key, "message_id"))
        {
          continue;
        }
      json_reader_read_member (reader, key);
      // key-value (e.g. for optional preferences)
      if (json_reader_is_value (reader))
        {
          JsonNode *node_value;
          char *write;
          GType type;
          node_value = json_reader_get_value (reader);
          type = json_node_get_value_type (node_value);

          if (type == G_TYPE_STRING)
            {
              const char *value = json_reader_get_string_value (reader);
              int len = strlen (value);
              write = (char *) (malloc (sizeof (char) * len + 1));
              sprintf (write, "%s", value);
            }
          if (type == G_TYPE_BOOLEAN)
            {
              const int value = json_reader_get_boolean_value (reader);
              write = value ? "yes\0" : "no\0";
            }
          if (type == G_TYPE_INT64 || type == G_TYPE_INT)
            {
              const int value = json_reader_get_int_value (reader);
              int buf = value;
              int len = 0;
              do
                {
                  buf /= 10;
                  len++;
                }
              while (buf);
              write = (char *) (malloc (sizeof (char) * len + 1));
              sprintf (write, "%d", value);
            }
          g_debug ("%s -> %s", key, write);
          prefs_set (key, write);
        }
      // list (ports, hosts)
      // parse list comma separated into single string
      if (json_reader_is_array (reader))
        {
          if (!strcmp (key, "hosts"))
            {
              key = "TARGET";
            }
          if (!strcmp (key, "ports"))
            {
              key = "port_range";
            }
          const char *value;
          char *values;
          int j;
          int len;

          int elements = json_reader_count_elements (reader);

          // Read first element
          if (elements > 0)
            {
              json_reader_read_element (reader, 0);
              value = json_reader_get_string_value (reader);
              len = strlen (value);
              values = (char *) (malloc (sizeof (char) * len + 1));
              sprintf (values, "%s", value);
              json_reader_end_element (reader);

              // Concatinate all other ellements comma separated
              for (j = 1; j < elements; j++)
                {
                  json_reader_read_element (reader, j);
                  value = json_reader_get_string_value (reader);
                  len += strlen (value);
                  char *buf = values;
                  values = (char *) (malloc (sizeof (char) * len + 1));
                  sprintf (values, "%s,%s", buf, value);
                  free (buf);
                  json_reader_end_element (reader);
                }
              g_debug ("%s -> %s", key, values);
              prefs_set (key, values);
            }
        }
      // dictionary
      // credentials, script preferences
      if (json_reader_is_object (reader))
        {
          if (!strcmp (key, "plugins"))
            {
              const char *plugin;
              char *plugins;
              int len, j;
              json_reader_read_member (reader, "single_vts");
              int num_plugins = json_reader_count_elements (reader);
              key = "plugin_set";

              if (num_plugins > 0)
                {
                  json_reader_read_element (reader, 0);
                  json_reader_read_member (reader, "oid");
                  plugin = json_reader_get_string_value (reader);
                  len = strlen (plugin);
                  plugins = (char *) (malloc (sizeof (char) * len + 1));
                  sprintf (plugins, "%s", plugin);
                  json_reader_end_member (reader);
                  json_reader_end_element (reader);
                  for (j = 1; j < num_plugins; j++)
                    {
                      json_reader_read_element (reader, j);
                      json_reader_read_member (reader, "oid");
                      plugin = json_reader_get_string_value (reader);
                      len += strlen (plugin);
                      char *buf = plugins;
                      plugins = (char *) (malloc (sizeof (char) * len + 1));
                      sprintf (plugins, "%s;%s", buf, plugin);
                      free (buf);
                      json_reader_end_member (reader);
                      json_reader_end_element (reader);
                    }
                  g_debug ("%s -> %s", key, plugins);
                  prefs_set (key, plugins);
                }
              json_reader_end_member (reader);
            }
        }
      json_reader_end_member (reader);
    }

  g_object_unref (reader);
  g_object_unref (parser);
  g_free (members);
  return 0;
}

/**
 * @brief Read the scan preferences from mqtt
 *
 * Adds preferences to the global_prefs.
 * If preference already exists in global_prefs they will be overwritten by
 * prefs from client.
 *
 * @param globals Scan ID of globals used as key to find the corresponding KB
 * where to take the preferences from. Globals also used for file upload.
 *
 * @return 0 on success, -1 if the kb is not found or no prefs are found in
 *         the kb.
 */
static int
overwrite_openvas_prefs_with_prefs_from_client (struct scan_globals *globals)
{
  char *msg_id;
  char *group_id;
  char *context = "eulabeia"; // TODO: get from prefs
  char *scan_id;
  time_t seconds;
  char topic_send[128];
  char msg_send[1024];

  char topic_sub[128];
  char *topic_recv;
  char *msg_recv;
  int topic_len;
  int msg_len;

  int ret;

  // Set a few defaults
  // TODO: Get alive test via mqtt
  prefs_set ("ALIVE_TEST", "2");

  // Subscribe to topic
  sprintf (topic_sub, "%s/scan/info", context);
  if (mqtt_subscribe (topic_sub))
    {
      g_message ("Subscription to %s failed", topic_sub);
    }
  g_message ("Successfully subscribed to %s", topic_sub);

  // Sned Get Scan
  msg_id = gvm_uuid_make ();
  group_id = gvm_uuid_make ();
  seconds = time (NULL); // TODO: Get time in Nanoseconds?
  scan_id = globals->scan_id;

  sprintf (topic_send, "%s/scan/cmd/director", context);
  sprintf (msg_send,
           "{\"message_id\":\"%s\","
           "\"group_id\":\"%s\","
           "\"message_type\":\"get.scan\","
           "\"created\":%d,"
           "\"id\":\"%s\"}",
           msg_id, group_id, (int) seconds, scan_id);

  mqtt_publish (topic_send, msg_send);
  // Wait for incomming data
  mqtt_retrieve_message (&topic_recv, &topic_len, &msg_recv, &msg_len);

  ret = write_json_preferences_recursive (msg_recv);

  free (msg_id);
  free (topic_recv);
  free (msg_recv);
  return ret;
}

/**
 * @brief Init logging.
 *
 * @return 0 on success, -1 on error.
 */
static int
init_logging ()
{
  static gchar *log_config_file_name = NULL;
  int err;

  log_config_file_name =
    g_build_filename (OPENVAS_SYSCONF_DIR, "openvas_log.conf", NULL);
  if (g_file_test (log_config_file_name, G_FILE_TEST_EXISTS))
    log_config = load_log_configuration (log_config_file_name);
  err = setup_log_handlers (log_config);
  if (err)
    {
      g_warning ("%s: Can not open or create log file or directory. "
                 "Please check permissions of log files listed in %s.",
                 __func__, log_config_file_name);
      g_free (log_config_file_name);
      return -1;
    }
  g_free (log_config_file_name);

  return 0;
}

static void
gcrypt_init (void)
{
  if (gcry_control (GCRYCTL_ANY_INITIALIZATION_P))
    return;
  gcry_check_version (NULL);
  gcry_control (GCRYCTL_SUSPEND_SECMEM_WARN);
  gcry_control (GCRYCTL_INIT_SECMEM, 16384, 0);
  gcry_control (GCRYCTL_RESUME_SECMEM_WARN);
  gcry_control (GCRYCTL_INITIALIZATION_FINISHED);
}

/**
 * @brief Check TLS.
 */
static void
check_tls ()
{
#if GNUTLS_VERSION_NUMBER < 0x030300
  if (openvas_SSL_init () < 0)
    g_message ("Could not initialize openvas SSL!");
#endif

  if (prefs_get ("debug_tls") != NULL && atoi (prefs_get ("debug_tls")) > 0)
    {
      g_warning ("TLS debug is enabled and should only be used with care, "
                 "since it may reveal sensitive information in the scanner "
                 "logs and might make openvas fill your disk rather quickly.");
      gnutls_global_set_log_function (my_gnutls_log_func);
      gnutls_global_set_log_level (atoi (prefs_get ("debug_tls")));
    }
}

/**
 * @brief Print start message.
 */
static void
openvas_print_start_msg ()
{
#ifdef OPENVAS_GIT_REVISION
  g_message ("openvas %s (GIT revision %s) started", OPENVAS_VERSION,
             OPENVAS_GIT_REVISION);
#else
  g_message ("openvas %s started", OPENVAS_VERSION);
#endif
}

/**
 * @brief Search in redis the process ID of a running scan and
 * sends it the kill signal SIGUSR1, which will stop the scan.
 * To find the process ID, it uses the scan_id passed with the
 * --scan-stop option.
 */
static void
stop_single_task_scan (void)
{
  char key[1024];
  kb_t kb;
  int pid;

  if (!global_scan_id)
    {
      exit (1);
    }

  snprintf (key, sizeof (key), "internal/%s", global_scan_id);
  kb = kb_find (prefs_get ("db_address"), key);
  if (!kb)
    {
      exit (1);
    }

  pid = kb_item_get_int (kb, "internal/ovas_pid");

  /* Only send the signal if the pid is a positive value.
     Since kb_item_get_int() will return -1 if the key does
     not exist. killing with -1 pid will send the signal system wide.
   */
  if (pid <= 0)
    return;

  /* Send the signal to the process group. */
  killpg (pid, SIGUSR1);
}

/**
 * @brief Set up data needed for attack_network().
 *
 * @param globals scan_globals needed for client preference handling.
 * @param config_file Used for config preference handling.
 */
void
attack_network_init (struct scan_globals *globals, const gchar *config_file)
{
  const char *mqtt_server_uri;

  set_default_openvas_prefs ();
  prefs_config (config_file);

  /* Init MQTT communication */
  mqtt_server_uri = prefs_get ("mqtt_server_uri");
  if (mqtt_server_uri)
    {
      if ((mqtt_init (mqtt_server_uri)) != 0)
        g_message ("%s: Failed init of MQTT communication.", __func__);
      else
        g_message ("%s: Successful init of MQTT communication.", __func__);
    }

  if (prefs_get ("vendor_version") != NULL)
    vendor_version_set (prefs_get ("vendor_version"));
  check_tls ();
  openvas_print_start_msg ();

  if (plugins_cache_init ())
    {
      g_message ("Failed to initialize nvti cache.");
      nvticache_reset ();
      exit (1);
    }
  nvticache_reset ();

  init_signal_handlers ();

  /* Make process a group leader, to make it easier to cleanup forked
   * processes & their children. */
  setpgid (0, 0);

  if (overwrite_openvas_prefs_with_prefs_from_client (globals))
    {
      g_warning ("No preferences found for the scan %s", globals->scan_id);
      exit (0);
    }
}

/**
 * @brief openvas.
 * @param argc Argument count.
 * @param argv Argument vector.
 */
int
openvas (int argc, char *argv[])
{
  int err;

  proctitle_init (argc, argv);
  gcrypt_init ();

  static gboolean display_version = FALSE;
  static gchar *config_file = NULL;
  static gchar *scan_id = NULL;
  static gchar *stop_scan_id = NULL;
  static gboolean print_specs = FALSE;
  static gboolean print_sysconfdir = FALSE;
  static gboolean update_vt_info = FALSE;
  GError *error = NULL;
  GOptionContext *option_context;
  static GOptionEntry entries[] = {
    {"version", 'V', 0, G_OPTION_ARG_NONE, &display_version,
     "Display version information", NULL},
    {"config-file", 'c', 0, G_OPTION_ARG_FILENAME, &config_file,
     "Configuration file", "<filename>"},
    {"cfg-specs", 's', 0, G_OPTION_ARG_NONE, &print_specs,
     "Print configuration settings", NULL},
    {"sysconfdir", 'y', 0, G_OPTION_ARG_NONE, &print_sysconfdir,
     "Print system configuration directory (set at compile time)", NULL},
    {"update-vt-info", 'u', 0, G_OPTION_ARG_NONE, &update_vt_info,
     "Updates VT info into redis store from VT files", NULL},
    {"scan-start", '\0', 0, G_OPTION_ARG_STRING, &scan_id,
     "ID of scan to start. ID and related data must be stored into redis "
     "before.",
     "<string>"},
    {"scan-stop", '\0', 0, G_OPTION_ARG_STRING, &stop_scan_id,
     "ID of scan to stop", "<string>"},

    {NULL, 0, 0, 0, NULL, NULL, NULL}};

  option_context =
    g_option_context_new ("- Open Vulnerability Assessment Scanner");
  g_option_context_add_main_entries (option_context, entries, NULL);
  if (!g_option_context_parse (option_context, &argc, &argv, &error))
    {
      g_print ("%s\n\n", error->message);
      exit (0);
    }
  g_option_context_free (option_context);

  /* --sysconfdir */
  if (print_sysconfdir)
    {
      g_print ("%s\n", SYSCONFDIR);
      exit (0);
    }

  /* --version */
  if (display_version)
    {
      printf ("OpenVAS %s\n", OPENVAS_VERSION);
#ifdef OPENVAS_GIT_REVISION
      printf ("GIT revision %s\n", OPENVAS_GIT_REVISION);
#endif
      printf ("gvm-libs %s\n", gvm_libs_version ());
      printf ("Most new code since 2005: (C) 2021 Greenbone Networks GmbH\n");
      printf (
        "Nessus origin: (C) 2004 Renaud Deraison <deraison@nessus.org>\n");
      printf ("License GPLv2: GNU GPL version 2\n");
      printf (
        "This is free software: you are free to change and redistribute it.\n"
        "There is NO WARRANTY, to the extent permitted by law.\n\n");
      exit (0);
    }

  /* Switch to UTC so that OTP times are always in UTC. */
  if (setenv ("TZ", "utc 0", 1) == -1)
    {
      g_print ("%s\n\n", strerror (errno));
      exit (0);
    }
  tzset ();

  if ((err = init_logging ()) != 0)
    return -1;

  err = init_sentry ();
  err ? /* Sentry is optional */
      : g_message ("Sentry is enabled. This can log sensitive information.");

  /* Config file location */
  if (!config_file)
    config_file = OPENVAS_CONF;

  if (update_vt_info)
    {
      set_default_openvas_prefs ();
      prefs_config (config_file);
      set_globals_from_preferences ();
      err = plugins_init ();
      nvticache_reset ();
      gvm_close_sentry ();
      return err ? -1 : 0;
    }

  /* openvas --scan-stop */
  if (stop_scan_id)
    {
      global_scan_id = g_strdup (stop_scan_id);
      stop_single_task_scan ();
      gvm_close_sentry ();
      exit (0);
    }

  /* openvas --scan-start */
  if (scan_id)
    {
      struct scan_globals *globals;
      global_scan_id = g_strdup (scan_id);
      globals = g_malloc0 (sizeof (struct scan_globals));
      globals->scan_id = g_strdup (global_scan_id);

      attack_network_init (globals, config_file);
      g_message ("attack_network_init successfully executed");
      attack_network (globals);

      gvm_close_sentry ();
      exit (0);
    }

  if (print_specs)
    {
      set_default_openvas_prefs ();
      prefs_config (config_file);
      prefs_dump ();
      gvm_close_sentry ();
      exit (0);
    }

  exit (0);
}
