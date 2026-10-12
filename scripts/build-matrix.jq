{
	include: [
		.[] as $t | ["debug", "release"][] | {
			profile: .,
			target: $t,
			# Each target on a runner of its architecture, where its tests run as they are.
			runner: (if ($t | startswith("aarch64-")) then "ubuntu-26.04-arm" else "ubuntu-26.04" end)
		}
	]
}
