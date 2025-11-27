target "default" {
  context = "./"
  dockerfile = "Dockerfile"
  args = {
    HELIOS_FEATURES = "balenahup"
  }
  platforms = [
    "linux/386",
    "linux/amd64",
    "linux/arm64",
    "linux/arm/v6",
    "linux/arm/v7",
  ]
}


target "unstable" {
  context = "./"
  dockerfile = "Dockerfile"
  args = {
    HELIOS_FEATURES = "all"
  }
  platforms = [
    "linux/386",
    "linux/amd64",
    "linux/arm64",
    "linux/arm/v6",
    "linux/arm/v7",
  ]
}
