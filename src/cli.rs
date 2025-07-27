use axum::http::Uri;
use clap::{arg, command, value_parser, Arg, ArgMatches};
use std::num::ParseIntError;
use std::time::Duration;

use crate::api::LocalAddress;
use crate::state::models::Uuid;

fn parse_duration(s: &str) -> Result<Duration, ParseIntError> {
    let millis: u64 = s.parse()?;
    Ok(Duration::from_millis(millis))
}

pub struct StartArgs {
    pub uuid: Option<Uuid>,
    pub local_api_address: Option<LocalAddress>,
    pub remote_api_endpoint: Option<Uri>,
    pub remote_api_key: Option<String>,
    pub remote_request_timeout: Option<Duration>,
    pub remote_poll_interval: Option<Duration>,
    pub remote_poll_min_interval: Option<Duration>,
    pub remote_poll_max_jitter: Option<Duration>,
    pub legacy_api_endpoint: Option<Uri>,
    pub legacy_api_key: Option<String>,
}

pub struct RegisterArgs {
    pub uuid: Option<Uuid>,
    pub remote_api_endpoint: Uri,
    pub remote_api_key: Option<String>,
    pub remote_request_timeout: Option<Duration>,
    pub remote_poll_interval: Option<Duration>,
    pub remote_poll_min_interval: Option<Duration>,
    pub remote_poll_max_jitter: Option<Duration>,
    pub provisioning_key: String,
    pub provisioning_fleet: u32,
    pub provisioning_device_type: String,
    pub provisioning_supervisor_version: Option<String>,
    pub provisioning_os_version: Option<String>,
    pub provisioning_os_variant: Option<String>,
    pub provisioning_mac_address: Option<String>,
}

pub enum Command {
    Start(Box<StartArgs>),
    Register(Box<RegisterArgs>),
}

pub fn parse() -> Command {
    let all_args = vec![
        arg!(--"uuid" <uuid> "Unique identifier for this device")
            .env("HELIOS_UUID")
            .value_parser(value_parser!(Uuid)),

        // Local API arguments

        arg!(--"local-api-address" <addr> "Local API listen address")
            .env("HELIOS_LOCAL_API_ADDRESS")
            .value_parser(value_parser!(LocalAddress)),

        // Remote arguments

        arg!(--"remote-api-endpoint" <uri> "Remote API endpoint URI")
            .env("HELIOS_REMOTE_API_ENDPOINT")
            .value_parser(value_parser!(Uri)),

        arg!(--"remote-api-key" <key> "API key to use for authentication with remote")
            .env("HELIOS_REMOTE_API_KEY")
            .value_parser(value_parser!(String))
            .requires("remote-api-endpoint"),

        // Remote request arguments

        arg!(--"remote-request-timeout" <ms> "Remote request timeout in milliseconds")
            .env("HELIOS_REMOTE_REQUEST_TIMEOUT")
            .value_parser(parse_duration)
            .requires("remote-api-endpoint"),

        arg!(--"remote-poll-interval" <ms> "Remote poll interval in milliseconds")
            .env("HELIOS_REMOTE_POLL_INTERVAL")
            .value_parser(parse_duration)
            .requires("remote-api-endpoint"),

        arg!(--"remote-poll-min-interval" <ms> "Remote rate limiting interval in milliseconds")
            .env("HELIOS_REMOTE_POLL_MIN_INTERVAL")
            .value_parser(parse_duration)
            .requires("remote-api-endpoint"),

        arg!(--"remote-poll-max-jitter" <ms> "Remote target state poll max jitter in milliseconds")
            .env("HELIOS_REMOTE_POLL_MAX_JITTER")
            .value_parser(parse_duration)
            .requires("remote-api-endpoint"),

        // Legacy Supervisor API arguments

        arg!(--"legacy-api-endpoint" <uri> "URI of legacy Supervisor API")
            .env("HELIOS_LEGACY_API_ENDPOINT")
            .value_parser(value_parser!(Uri))
            .requires("legacy-api-key"),

        arg!(--"legacy-api-key" <key> "API key for authentication with legacy Supervisor API")
            .env("HELIOS_LEGACY_API_KEY")
            .value_parser(value_parser!(String))
            .requires("legacy-api-endpoint"),

        // Provisioning arguments

        arg!(--"provisioning-key" <key> "Provisioning key to use for authenticating with remote during registration")
            .env("HELIOS_PROVISIONING_KEY")
            .value_parser(value_parser!(String))
            .requires("remote-api-endpoint")
            .requires("provisioning-fleet")
            .requires("provisioning-device-type"),

        arg!(--"provisioning-fleet" <int> "ID of the fleet to provision this device into")
            .env("HELIOS_PROVISIONING_FLEET")
            .value_parser(value_parser!(u32))
            .requires("provisioning-key"),

        arg!(--"provisioning-device-type" <slug> "Device type")
            .env("HELIOS_PROVISIONING_DEVICE_TYPE")
            .value_parser(value_parser!(String))
            .requires("provisioning-key"),

        arg!(--"provisioning-supervisor-version" <str> "Supervisor version")
            .env("HELIOS_PROVISIONING_SUPERVISOR_VERSION")
            .value_parser(value_parser!(String))
            .requires("provisioning-key"),

        arg!(--"provisioning-os-version" <str> "Host OS name and version, eg. \"balenaOS 6.5.39+rev1\"")
            .env("HELIOS_PROVISIONING_OS_VERSION")
            .value_parser(value_parser!(String))
            .requires("provisioning-key"),

        arg!(--"provisioning-os-variant" <str> "Host OS variant")
            .env("HELIOS_PROVISIONING_OS_VARIANT")
            .value_parser(["prod", "dev"])
            .requires("provisioning-key"),

        arg!(--"provisioning-mac-address" <str> "MAC address")
            .env("HELIOS_PROVISIONING_MAC_ADDRESS")
            .value_parser(value_parser!(String))
            .requires("provisioning-key"),
    ];

    /// Clones and returns the argument with the provided long name.
    ///
    /// See array of args above.
    fn get(args: &[Arg], long_name: &str) -> Arg {
        for arg in args {
            if arg.get_id().eq(long_name) {
                return arg.clone();
            }
        }
        // args are right there, just make sure you type the name correctly
        unreachable!("argument with {long_name} not found");
    }

    let cli = command!()
        .subcommand_required(true)
        .subcommand(clap::Command::new("start").args([
            get(&all_args, "uuid"),
            get(&all_args, "local-api-address"),
            get(&all_args, "remote-api-endpoint"),
            get(&all_args, "remote-api-key"),
            get(&all_args, "remote-request-timeout"),
            get(&all_args, "remote-poll-interval"),
            get(&all_args, "remote-poll-min-interval"),
            get(&all_args, "remote-poll-max-jitter"),
            get(&all_args, "legacy-api-endpoint"),
            get(&all_args, "legacy-api-key"),
        ]))
        .subcommand(clap::Command::new("register").args([
            get(&all_args, "uuid"),
            get(&all_args, "remote-api-endpoint").required(true),
            get(&all_args, "remote-api-key"),
            get(&all_args, "remote-request-timeout"),
            get(&all_args, "remote-poll-interval"),
            get(&all_args, "remote-poll-min-interval"),
            get(&all_args, "remote-poll-max-jitter"),
            get(&all_args, "provisioning-key").required(true),
            get(&all_args, "provisioning-fleet"),
            get(&all_args, "provisioning-device-type"),
            get(&all_args, "provisioning-supervisor-version"),
            get(&all_args, "provisioning-os-version"),
            get(&all_args, "provisioning-os-variant"),
            get(&all_args, "provisioning-mac-address"),
        ]));

    let matches = cli.get_matches();

    fn req<T: Clone + Send + Sync + 'static>(matches: &ArgMatches, long_name: &str) -> T {
        matches.get_one::<T>(long_name).cloned().unwrap()
    }

    fn opt<T: Clone + Send + Sync + 'static>(matches: &ArgMatches, long_name: &str) -> Option<T> {
        matches.get_one::<T>(long_name).cloned()
    }

    match matches.subcommand() {
        Some(("start", args)) => Command::Start(Box::new(StartArgs {
            uuid: opt(args, "uuid"),
            local_api_address: opt(args, "local-api-address"),
            remote_api_endpoint: opt(args, "remote-api-endpoint"),
            remote_api_key: opt(args, "remote-api-key"),
            remote_request_timeout: opt(args, "remote-request-timeout"),
            remote_poll_interval: opt(args, "remote-poll-interval"),
            remote_poll_min_interval: opt(args, "remote-poll-min-interval"),
            remote_poll_max_jitter: opt(args, "remote-poll-max-jitter"),
            legacy_api_endpoint: opt(args, "legacy-api-endpoint"),
            legacy_api_key: opt(args, "legacy-api-key"),
        })),
        Some(("register", args)) => Command::Register(Box::new(RegisterArgs {
            uuid: opt(args, "uuid"),
            remote_api_endpoint: req(args, "remote-api-endpoint"),
            remote_api_key: opt(args, "uuid"),
            remote_request_timeout: opt(args, "uuid"),
            remote_poll_interval: opt(args, "remote-poll-interval"),
            remote_poll_min_interval: opt(args, "remote-poll-min-interval"),
            remote_poll_max_jitter: opt(args, "remote-poll-max-jitter"),
            provisioning_key: req(args, "provisioning-key"),
            provisioning_fleet: req(args, "provisioning-fleet"),
            provisioning_device_type: req(args, "provisioning-device-type"),
            provisioning_supervisor_version: opt(args, "provisioning-supervisor-version"),
            provisioning_os_version: opt(args, "provisioning-os-version"),
            provisioning_os_variant: opt(args, "provisioning-os-variant"),
            provisioning_mac_address: opt(args, "provisioning-mac-address"),
        })),
        _ => unreachable!("`subcommand_required(true)` prevents `None`"),
    }
}
