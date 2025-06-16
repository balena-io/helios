# theseus

Experimental replacement for the [balenaSupervisor](https://github.com/balena-os/balenas-supervisor) written in Rust. This service, when installed, will reconfigure the existing supervisor and act as a proxy for requests coming to and from the supervisor. This feature will allow us to slowly replace the legacy supervisor with a more modern and lean implementation.

## Running

This requires the supervisor version from [balena-os/balena-supervisor#2422](https://github.com/balena-os/balena-supervisor/pull/2422).

To install the draft supervisor run the following commands on the host of your balena device making sure to select the id for the right architecture.

```sh
# Set necessary variables
api_key=$(cat /mnt/boot/config.json | jq -r .deviceApiKey)
uuid=$(cat /mnt/boot/config.json | jq -r .uuid)
api_endpoint=$(cat /mnt/boot/config.json | jq -r .apiEndpoint)

# Supervisor releases
amd64_supervisor=3530898
aarch64_supervisor=3530895
# Make sure to use the right id for your architecture here
supervisor_release="${aarch64_supervisor}"

supervisor_img=$(\
  curl -X GET \
    "$api_endpoint/v7/release($supervisor_release)?\$expand=contains__image(\$expand=image(\$select=is_stored_at__image_location))" \
    -H  "Content-Type: application/json" \
    -H "Authorization: Bearer $api_key" | jq -r .d[0].contains__image[0].image[0].is_stored_at__image_location \
)

# Patch the device to the draft release
curl -X PATCH -H "Authorization: Bearer $api_key" -H  "Content-Type: application/json" "$api_endpoint/v6/device?\$filter=uuid%20eq%20'$uuid'" -d "{\"should_be_managed_by__supervisor_release\": $supervisor_release}"

# Update the supervisor to the target image
update-balena-supervisor -i "$supervisor_img"
```

With the supervisor in local mode, you can now use livepush with the [balena CLI](https://docs.balena.io/reference/balena-cli/latest/) to run next-balena-supervisor on your local device.

```sh
balena push <local ip>
```

If successful, the service will restart the existing supervisor and become a proxy for all connections between the device supervisor and the API.
