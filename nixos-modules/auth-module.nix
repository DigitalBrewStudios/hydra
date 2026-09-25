{
  config,
  pkgs,
  lib,
  ...
}:
let
  inherit (lib) types mkOption;

  cfg = config.services.hydra-auth-dev;
  user = "hydra-auth";

  format = pkgs.formats.toml { };
  otel = import ./otel.nix { inherit lib; };

  oauthishProvider = {
    options = {
      type = mkOption {
        type = types.singleLineStr;
        description = "The type of authentication method used by the provider.";
      };
      clientId = mkOption {
        type = types.singleLineStr;
        description = "The provider Client ID.";
      };
      clientSecretPath = mkOption {
        type = types.path;
        description = "Path to the provider Client Secret.";
      };
      scopes = mkOption {
        type = types.listOf types.singleLineStr;
        default = [ ];
        description = "Additional scopes to request.";
      };
      redirectUrl = mkOption {
        type = types.nullOr types.singleLineStr;
        default = null;
        description = "The OAuth2/OIDC redirect URL, if it cannot be derived.";
      };
    };
  };

  oidcProvider = {
    options = {
      type = mkOption {
        type = types.enum [ "OIDC" ];
        description = "OpenID Connect provider.";
      };
      issuerUrl = mkOption {
        type = types.singleLineStr;
        description = "The OIDC issuer URL (e.g. https://id.example.com/realms/example).";
      };
      usernameClaim = mkOption {
        type = types.singleLineStr;
        default = "preferred_username";
        description = "The claim used as the username.";
      };
      groupsClaim = mkOption {
        type = types.nullOr types.singleLineStr;
        default = null;
        description = "The claim holding a list of the user's groups.";
      };
    };
  };
in
{
  options = {
    services.hydra-auth-dev = {
      enable = lib.mkEnableOption "hydra-auth, a backend service that presents Hydra authentication.";

      settings = mkOption {
        description = ''
          Settings for hydra-auth, written to
          `/etc/hydra/auth.toml`.

          Every Rust service in hydra has its own separate TOML configuration file,
          with just the settings it needs.
        '';
        type = types.submodule {
          options = {
            dbUrl = mkOption {
              description = "Postgresql database url";
              type = types.singleLineStr;
              default = "postgres://hydra@%2Frun%2Fpostgresql:5432/hydra";
            };
            maxDbConnections = mkOption {
              description = "Postgresql maximum db connections";
              type = types.ints.positive;
              default = 4;
            };

            providers = mkOption {
              description = ''
                Authentication Provider Configuration
              '';
              type = lib.types.lazyAttrsOf lib.types.submoduleWith {
                modules = [
                  # Extensible
                  (
                    { name }:
                    {
                      options = {
                        name = mkOption {
                          type = types.singleLineStr;
                          default = name;
                          description = "The name of the provider.";
                        };
                        type = mkOption {
                          type = types.enum [
                            "GitHub"
                            "OIDC"
                            "LDAP"
                            "SAML"
                          ];
                          description = "The type of authentication method used to hook up the provider.";
                        };

                        hydraLoginText = mkOption {
                          type = types.singleLineStr;
                          description = "The login text that shows in hydra.";
                          default = "Login in with ${name}.";
                        };
                      };
                    }
                  )

                  # GitHub
                  {
                    options = {
                      type = mkOption {
                        type = types.enum [ "GitHub" ];
                      };
                      clientId = mkOption {
                        type = types.singleLineStr;
                        description = "The provider Client ID.";
                      };
                      clientSecretPath = mkOption {
                        type = types.path;
                        description = "Path to the provider Client Secret.";
                      };
                    };
                  }

                  # OIDC
                  {
                    options = {
                      type = mkOption {
                        type = types.enum [ "OIDC" ];
                        description = "OpenID Connect provider.";
                      };
                      issuerUrl = mkOption {
                        type = types.singleLineStr;
                        description = "The OIDC issuer URL (e.g. https://id.example.com/realms/example).";
                      };
                      usernameClaim = mkOption {
                        type = types.singleLineStr;
                        default = "preferred_username";
                        description = "The claim used as the username.";
                      };
                      groupsClaim = mkOption {
                        type = types.nullOr types.singleLineStr;
                        default = null;
                        description = "The claim holding a list of the user's groups.";
                      };
                    };
                  }

                  # LDAP
                  {
                    options = {
                      type = mkOption {
                        type = types.enum [ "LDAP" ];
                      };
                      url = mkOption {
                        type = types.singleLineStr;
                        description = "The LDAP server URL (ldap:// or ldaps://).";
                      };
                      startTls = mkOption {
                        type = types.bool;
                        default = false;
                        description = "Upgrade the connection with STARTTLS.";
                      };
                      requireValidCert = mkOption {
                        type = types.bool;
                        default = true;
                        description = "Whether to require a valid TLS certificate.";
                      };
                      bindDn = mkOption {
                        type = types.nullOr types.singleLineStr;
                        default = null;
                        description = "The DN to bind with before searching. Leave null for anonymous binds.";
                      };
                      bindPasswordPath = mkOption {
                        type = types.nullOr types.path;
                        default = null;
                        description = "Path to the bind password. Only used when bindDn is set.";
                      };
                      userBaseDn = mkOption {
                        type = types.singleLineStr;
                        description = "The subtree to search for users.";
                      };
                      userFilter = mkOption {
                        type = types.singleLineStr;
                        default = "(&(objectClass=posixAccount)(uid=%s))";
                        description = "The LDAP filter for finding users, with %s as the placeholder for the username.";
                      };
                      userAttr = mkOption {
                        type = types.singleLineStr;
                        default = "uid";
                        description = "The attribute holding the username.";
                      };
                      emailAttr = mkOption {
                        type = types.singleLineStr;
                        default = "mail";
                        description = "The attribute holding the user's email address.";
                      };
                      groupBaseDn = mkOption {
                        type = types.nullOr types.singleLineStr;
                        default = null;
                        description = "The subtree to search for groups. Leave null to skip group lookups.";
                      };
                      groupFilter = mkOption {
                        type = types.singleLineStr;
                        default = "(objectClass=posixGroup)";
                        description = "The LDAP filter for finding groups.";
                      };
                      groupAttr = mkOption {
                        type = types.singleLineStr;
                        default = "cn";
                        description = "The attribute holding the group name.";
                      };
                      memberAttr = mkOption {
                        type = types.singleLineStr;
                        default = "member";
                        description = "The attribute listing the group members.";
                      };
                    };
                  }

                  # SAML
                  {
                    options = {
                      type = mkOption {
                        type = types.enum [ "SAML" ];
                      };
                      metadataUrl = mkOption {
                        type = types.nullOr types.singleLineStr;
                        default = null;
                        description = "The IdP SAML metadata URL. Mutually exclusive with metadataXmlPath.";
                      };
                      metadataXmlPath = mkOption {
                        type = types.nullOr types.path;
                        default = null;
                        description = "Path to the IdP SAML metadata XML. Mutually exclusive with metadataUrl.";
                      };
                      entityId = mkOption {
                        type = types.singleLineStr;
                        description = "The entity ID (issuer) of this service provider.";
                      };
                      assertionConsumerServiceUrl = mkOption {
                        type = types.singleLineStr;
                        description = "The ACS URL this service listens on.";
                      };
                      nameIdFormat = mkOption {
                        type = types.nullOr types.singleLineStr;
                        default = null;
                        description = "The requested NameID format.";
                      };
                      usernameAttr = mkOption {
                        type = types.singleLineStr;
                        default = "uid";
                        description = "The assertion attribute holding the username.";
                      };
                      emailAttr = mkOption {
                        type = types.singleLineStr;
                        default = "email";
                        description = "The assertion attribute holding the user's email address.";
                      };
                      groupsAttr = mkOption {
                        type = types.nullOr types.singleLineStr;
                        default = null;
                        description = "The assertion attribute holding a list of the user's groups.";
                      };
                      wantAssertionsSigned = mkOption {
                        type = types.bool;
                        default = true;
                        description = "Whether IdP assertions must be signed.";
                      };
                      spPrivateKeyPath = mkOption {
                        type = types.nullOr types.path;
                        default = null;
                        description = "Path to the service provider private key, used for signing AuthnRequests.";
                      };
                      spCertificatePath = mkOption {
                        type = types.nullOr types.path;
                        default = null;
                        description = "Path to the service provider certificate. Required when spPrivateKeyPath is set.";
                      };
                    };
                  }
                ];
              };
            };
          };
        };
        default = { };
      };

      otel = otel.mkOtelOption {
        component = "hydra-auth";
        binary = "hydra-auth";
      };

      package = mkOption {
        type = types.package;
        # `withOtel` is a knob on the rust workspace, not on this crate: cargo
        # resolves features once for the whole workspace build.
        default = (pkgs.hydraComponents.overrideScope (_: _: { withOtel = cfg.otel.enable; })).hydra-auth;
        defaultText = lib.literalExpression "pkgs.hydraComponents.hydra-auth";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.services.hydra-auth-dev = {
      description = "Hydra Authentication service";

      requires = [ "hydra-auth-dev.socket" ];
      after = [
        "hydra-init.service"
        "network.target"
      ];
      wantedBy = [ "multi-user.target" ];

      serviceConfig = {
        Type = "notify";
        Restart = "always";
        RestartSec = "5s";

        ExecStart = lib.escapeShellArgs [
          "${cfg.package}/bin/hydra-auth"
          "--config-path"
          "${format.generate "hydra-auth.toml" (lib.filterAttrsRecursive (_: v: v != null) cfg.settings)}"
        ];

        User = user;
        Group = "hydra";

        PrivateNetwork = false;
        SystemCallFilter = [
          "@system-service"
          "~@privileged"
          "~@resources"
        ];

        ReadWritePaths = lib.optionals (lib.hasInfix "%2Frun%2Fpostgresql" cfg.settings.dbUrl) [
          "/run/postgresql/.s.PGSQL.${toString config.services.postgresql.settings.port}"
        ];
        RuntimeDirectory = "hydra-auth";

        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        ProtectKernelTunables = true;
        ProtectControlGroups = true;
        RestrictSUIDSGID = true;
        PrivateMounts = true;
        RemoveIPC = true;
        UMask = "0022";

        CapabilityBoundingSet = "";
        NoNewPrivileges = true;

        ProtectKernelModules = true;
        SystemCallArchitectures = "native";
        ProtectKernelLogs = true;
        ProtectClock = true;

        RestrictAddressFamilies = "";

        LockPersonality = true;
        ProtectHostname = true;
        RestrictRealtime = true;
        MemoryDenyWriteExecute = true;
        PrivateUsers = true;
        RestrictNamespaces = true;
      };
    };

    systemd.sockets.hydra-auth-dev = {
      description = "Hydra Authentication Backend";
      wantedBy = [ "sockets.target" ];
      socketConfig = {
        ListenStream = "${cfg.bind.address}:${toString cfg.bind.port}";
        FileDescriptorName = "ws";
        Service = "hydra-auth-dev.service";
      };
    };
    #
    # services.postgresql.identMap = ''
    #   hydra-users ${user} hydra
    # '';

    users = {
      groups.hydra = { };
      users.${user} = {
        group = "hydra";
        isSystemUser = true;
      };
    };
  };
}
