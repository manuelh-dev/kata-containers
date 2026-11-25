# Copyright (c) NVIDIA Corporation
#
# SPDX-License-Identifier: Apache-2.0

apiVersion: v1
kind: Namespace
metadata:
  name: kata-initdata-webhook
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: kata-initdata-webhook
  namespace: kata-initdata-webhook
spec:
  replicas: 1
  selector:
    matchLabels:
      app: kata-initdata-webhook
  template:
    metadata:
      labels:
        app: kata-initdata-webhook
    spec:
      containers:
        - name: webhook
          image: ${WEBHOOK_IMAGE}
          imagePullPolicy: Never
          args:
            - -tls-cert-file=/etc/webhook/certs/cert.pem
            - -tls-key-file=/etc/webhook/certs/key.pem
            - -only-namespaces-regexp=^default$
            - -initdata-runtime-classes=${INITDATA_RUNTIME_CLASSES}
            - -agent-policy-mode=${AGENT_POLICY_MODE}
            - -agent-policy-file=/etc/webhook/agent-policy/policy.rego
            - -oci-settings-file=${WEBHOOK_OCI_SETTINGS_FILE}
            - -cluster-settings-file=${WEBHOOK_CLUSTER_SETTINGS_FILE}
          ports:
            - name: https
              containerPort: 8080
          readinessProbe:
            tcpSocket:
              port: https
          securityContext:
            allowPrivilegeEscalation: false
            capabilities:
              drop:
                - ALL
            readOnlyRootFilesystem: true
            runAsNonRoot: true
            runAsUser: 1000
            seccompProfile:
              type: RuntimeDefault
          volumeMounts:
            - name: certs
              mountPath: /etc/webhook/certs
              readOnly: true
            - name: agent-policy
              mountPath: /etc/webhook/agent-policy
              readOnly: true
            - name: oci-settings
              mountPath: /etc/webhook/oci-settings
              readOnly: true
            - name: cluster-settings
              mountPath: /etc/webhook/cluster-settings
              readOnly: true
      volumes:
        - name: certs
          secret:
            secretName: pod-annotate-webhook-certs
        - name: agent-policy
          configMap:
            name: kata-initdata-webhook-agent-policy
        - name: oci-settings
          configMap:
            name: kata-initdata-webhook-oci-settings
            optional: true
        - name: cluster-settings
          configMap:
            name: kata-initdata-webhook-cluster-settings
            optional: true
---
apiVersion: v1
kind: Service
metadata:
  name: pod-annotate-webhook
  namespace: kata-initdata-webhook
spec:
  selector:
    app: kata-initdata-webhook
  ports:
    - port: 443
      targetPort: https
---
apiVersion: admissionregistration.k8s.io/v1
kind: MutatingWebhookConfiguration
metadata:
  name: kata-initdata-webhook
webhooks:
  - name: initdata.kata-containers.io
    admissionReviewVersions:
      - v1
    sideEffects: None
    failurePolicy: Fail
    reinvocationPolicy: IfNeeded
    timeoutSeconds: 10
    namespaceSelector:
      matchLabels:
        kata-containers.io/initdata-webhook: enabled
    matchConditions:
      - name: initdata-runtime-class
        expression: >-
          has(object.spec.runtimeClassName) &&
          object.spec.runtimeClassName in ${INITDATA_RUNTIME_CLASSES_CEL}
    clientConfig:
      service:
        name: pod-annotate-webhook
        namespace: kata-initdata-webhook
        path: /mutate
      caBundle: ${CA_BUNDLE}
    rules:
      - operations:
          - CREATE
        apiGroups:
          - ""
        apiVersions:
          - v1
        resources:
          - pods
